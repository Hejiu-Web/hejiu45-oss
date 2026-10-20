//! Object writes that originate from the **console** rather than the S3 API.
//!
//! ## Why a second write path
//!
//! Console auth is a cookie session and S3 auth is SigV4, and those two paths
//! are deliberately isolated (see `src/admin/api.rs`). A browser therefore
//! cannot simply `PUT` to `/s3/...`: it holds no access key. Rather than force
//! an operator to mint a key just to drag a file in, this module performs the
//! write on the session's behalf.
//!
//! ## What authorises it
//!
//! The console session proves *who* is acting; this module then requires that
//! the account is an **admin** (`session::require` already enforces that) and
//! that the bucket exists. Admins are not subject to per-key bucket grants —
//! those exist to scope machine credentials — so no grant lookup is needed, and
//! that is the deliberate difference from the S3 path.
//!
//! ## What it shares with the S3 path
//!
//! Bodies still land through [`crate::data::Data`], so the on-disk layout,
//! atomic replace, hashing and the size limit are identical. Only the
//! authentication and request-shape handling differ.

use std::collections::HashMap;

use axum::Json;
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

use crate::admin::session;
use crate::data::validate_object_key;
use crate::error::ApiError;
use crate::meta::{
    BUCKETS, BucketMeta, MULTIPARTS, MultipartUpload, OBJECTS, ObjectMeta, composite_key,
};
use crate::s3::handlers::{adjust_bucket_stats, load_bucket};
use crate::s3::object::{MAX_PART_BYTES, build_object_meta, finalize_put};
use crate::state::{AppState, now_rfc3339};

#[derive(Serialize)]
pub struct UploadResult {
    pub key: String,
    pub size: u64,
    pub etag: String,
    pub content_type: String,
}

/// `POST /api/buckets/{name}/objects?key=...` — single-request upload.
///
/// The body is the raw file. Metadata comes from headers so a browser can send
/// the file's real `Content-Type` without re-encoding the bytes.
pub async fn put_object(
    State(st): State<AppState>,
    jar: axum_extra::extract::cookie::CookieJar,
    Path(name): Path<String>,
    Query(params): Query<HashMap<String, String>>,
    req: axum::http::Request<Body>,
) -> Result<Response, ApiError> {
    let user = session::require(&st.meta, &jar)?;
    let key = params
        .get("key")
        .cloned()
        .ok_or_else(|| ApiError::bad_request("Missing `key` query parameter."))?;
    validate_object_key(&key).map_err(|e| ApiError::bad_request(format!("{e:?}")))?;
    load_bucket(&st, &name).map_err(ApiError::from)?;

    let headers = req.headers().clone();
    let declared = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    let limits = st.limits();
    if let (Some(size), Some(max)) = (declared, limits.max_object_bytes) {
        if size > max {
            return Err(ApiError::too_large());
        }
    }

    let reader = body_reader(req);
    let stored = st
        .data
        .stage(reader, limits.max_object_bytes)
        .await
        .map_err(|e| {
            // A body over the limit surfaces as io::Error from `stage`.
            if format!("{e:?}").contains("too large") || format!("{e:?}").contains("limit") {
                ApiError::too_large()
            } else {
                ApiError::internal(format!("{e:?}"))
            }
        })?;

    // Reuse the S3 metadata builder so content sniffing and the generic-type
    // override rules stay in one place.
    let mut meta = build_object_meta(&key, &name, &stored, &headers);
    meta.multipart = false;
    finalize_put(&st, &name, &meta, &stored.staged)
        .await
        .map_err(ApiError::from)?;

    st.meta.audit(
        &user.user.username,
        "console.upload",
        &format!("{name}/{key}"),
        &format!("{} bytes", stored.size),
        "",
    );

    Ok(Json(UploadResult {
        key: meta.key,
        size: meta.size,
        etag: meta.md5_hex,
        content_type: meta.content_type,
    })
    .into_response())
}

// ---------------------------------------------------------------------------
// Multipart upload, driven by the browser for large files
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct InitResult {
    pub upload_id: String,
    pub part_bytes: u64,
}

#[derive(Serialize)]
pub struct PartResult {
    pub part_number: u32,
    pub etag: String,
}

#[derive(Deserialize)]
pub struct PartEntry {
    pub part_number: u32,
    pub etag: String,
}

#[derive(Deserialize)]
pub struct CompleteRequest {
    pub parts: Vec<PartEntry>,
}

fn upload_key(bucket: &str, id: &str) -> String {
    format!("{bucket}\u{0}{id}")
}

/// `POST /api/buckets/{name}/uploads?key=...` — begin a browser-driven upload.
pub async fn init_upload(
    State(st): State<AppState>,
    jar: axum_extra::extract::cookie::CookieJar,
    Path(name): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<InitResult>, ApiError> {
    let user = session::require(&st.meta, &jar)?;
    let key = params
        .get("key")
        .cloned()
        .ok_or_else(|| ApiError::bad_request("Missing `key` query parameter."))?;
    validate_object_key(&key).map_err(|e| ApiError::bad_request(format!("{e:?}")))?;
    load_bucket(&st, &name).map_err(ApiError::from)?;

    let upload_id = format!("{}{}", uuid::Uuid::new_v4().simple(), st.started_unix);
    let record = MultipartUpload {
        upload_id: upload_id.clone(),
        bucket: name.clone(),
        key: key.clone(),
        initiated_at: time::OffsetDateTime::now_utc().unix_timestamp(),
        content_type: params.get("content_type").cloned().unwrap_or_default(),
        user_meta: Default::default(),
        headers: Default::default(),
        parts: Default::default(),
    };
    st.meta
        .put_json(MULTIPARTS, &upload_key(&name, &upload_id), &record)
        .map_err(ApiError::internal)?;
    st.meta.audit(
        &user.user.username,
        "console.upload.begin",
        &format!("{name}/{key}"),
        "",
        "",
    );

    Ok(Json(InitResult {
        upload_id,
        part_bytes: st.limits().part_bytes,
    }))
}

/// `PUT /api/buckets/{name}/uploads/{id}/parts/{n}` — send one part.
pub async fn put_part(
    State(st): State<AppState>,
    jar: axum_extra::extract::cookie::CookieJar,
    Path((name, upload_id, part_number)): Path<(String, String, u32)>,
    req: axum::http::Request<Body>,
) -> Result<Json<PartResult>, ApiError> {
    session::require(&st.meta, &jar)?;
    if !(1..=10_000).contains(&part_number) {
        return Err(ApiError::bad_request(
            "part_number must be between 1 and 10000.",
        ));
    }
    // The upload must exist and belong to this bucket, so an upload id cannot
    // be used to write into another bucket.
    let up: MultipartUpload = st
        .meta
        .get_json(MULTIPARTS, &upload_key(&name, &upload_id))
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("Upload not found or already completed."))?;
    if up.bucket != name {
        return Err(ApiError::not_found("Upload not found."));
    }

    let reader = body_reader(req);
    let stored = st
        .data
        .stage(reader, Some(MAX_PART_BYTES))
        .await
        .map_err(|e| ApiError::internal(format!("{e:?}")))?;

    let dest = st.data.multipart_part_path(&upload_id, part_number);
    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(ApiError::internal)?;
    }
    tokio::fs::rename(&stored.staged, &dest)
        .await
        .map_err(ApiError::internal)?;

    // Atomic read-modify-write: the browser uploads parts concurrently.
    let md5 = stored.md5_hex.clone();
    st.meta
        .update_json::<MultipartUpload, _>(MULTIPARTS, &upload_key(&name, &upload_id), |u| {
            u.parts.insert(part_number, md5);
        })
        .map_err(ApiError::internal)?;

    Ok(Json(PartResult {
        part_number,
        etag: stored.md5_hex,
    }))
}

/// `POST /api/buckets/{name}/uploads/{id}/complete` — assemble the object.
pub async fn complete_upload(
    State(st): State<AppState>,
    jar: axum_extra::extract::cookie::CookieJar,
    Path((name, upload_id)): Path<(String, String)>,
    Json(body): Json<CompleteRequest>,
) -> Result<Json<UploadResult>, ApiError> {
    let user = session::require(&st.meta, &jar)?;
    let up: MultipartUpload = st
        .meta
        .get_json(MULTIPARTS, &upload_key(&name, &upload_id))
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("Upload not found."))?;

    if body.parts.is_empty() {
        return Err(ApiError::bad_request("No parts supplied."));
    }
    let mut requested: Vec<u32> = body.parts.iter().map(|p| p.part_number).collect();
    requested.sort_unstable();
    let expected: Vec<u32> = (1..=requested.len() as u32).collect();
    if requested != expected {
        return Err(ApiError::bad_request(
            "Parts must be numbered 1..n without gaps.",
        ));
    }

    let staged = st.data.new_tmp_path();
    let mut out = tokio::fs::File::create(&staged)
        .await
        .map_err(ApiError::internal)?;
    let mut total: u64 = 0;
    for n in &requested {
        let path = st.data.multipart_part_path(&upload_id, *n);
        let mut part = tokio::fs::File::open(&path)
            .await
            .map_err(|_| ApiError::bad_request(format!("Part {n} is missing on disk.")))?;
        total += tokio::io::copy(&mut part, &mut out)
            .await
            .map_err(ApiError::internal)?;
    }
    out.sync_all().await.map_err(ApiError::internal)?;
    drop(out);

    let limits = st.limits();
    if !limits.allows(total) {
        let _ = tokio::fs::remove_file(&staged).await;
        return Err(ApiError::too_large());
    }

    let (sha, md5) = crate::data::hash_file(&staged)
        .await
        .map_err(ApiError::internal)?;
    // Sniff from the first bytes of the assembled file, so a multipart upload
    // gets the same content type a single PUT would.
    let mut head = vec![0u8; 512];
    let sniffed = match tokio::fs::File::open(&staged).await {
        Ok(mut f) => {
            use tokio::io::AsyncReadExt;
            let n = f.read(&mut head).await.unwrap_or(0);
            crate::data::sniff_content_type(&head[..n])
        }
        Err(_) => "application/octet-stream",
    };
    let stored = crate::data::StoredObject {
        staged,
        size: total,
        sha256: sha,
        md5_hex: md5,
        sniffed_content_type: sniffed,
    };

    let mut headers = HeaderMap::new();
    if !up.content_type.is_empty() {
        if let Ok(v) = header::HeaderValue::from_str(&up.content_type) {
            headers.insert(header::CONTENT_TYPE, v);
        }
    }
    let mut meta = build_object_meta(&up.key, &name, &stored, &headers);
    meta.multipart = true;
    finalize_put(&st, &name, &meta, &stored.staged)
        .await
        .map_err(ApiError::from)?;

    // Best-effort cleanup of the part directory and the bookkeeping row.
    let _ = tokio::fs::remove_dir_all(st.data.multipart_dir(&upload_id)).await;
    st.meta
        .delete_row(MULTIPARTS, &upload_key(&name, &upload_id))
        .ok();
    st.meta.audit(
        &user.user.username,
        "console.upload.complete",
        &format!("{name}/{}", up.key),
        &format!("{total} bytes"),
        "",
    );

    Ok(Json(UploadResult {
        key: meta.key,
        size: meta.size,
        etag: meta.md5_hex,
        content_type: meta.content_type,
    }))
}

/// `DELETE /api/buckets/{name}/uploads/{id}` — abandon an upload.
pub async fn abort_upload(
    State(st): State<AppState>,
    jar: axum_extra::extract::cookie::CookieJar,
    Path((name, upload_id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    session::require(&st.meta, &jar)?;
    st.meta
        .delete_row(MULTIPARTS, &upload_key(&name, &upload_id))
        .map_err(ApiError::internal)?;
    let _ = tokio::fs::remove_dir_all(st.data.multipart_dir(&upload_id)).await;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Delete + download
// ---------------------------------------------------------------------------

/// `DELETE /api/buckets/{name}/objects?key=...`
pub async fn delete_object(
    State(st): State<AppState>,
    jar: axum_extra::extract::cookie::CookieJar,
    Path(name): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<StatusCode, ApiError> {
    let user = session::require(&st.meta, &jar)?;
    let key = params
        .get("key")
        .cloned()
        .ok_or_else(|| ApiError::bad_request("Missing `key` query parameter."))?;
    let existing: Option<ObjectMeta> = st
        .meta
        .get_json(OBJECTS, &composite_key(&name, &key))
        .map_err(ApiError::internal)?;
    // Deleting a missing object is a no-op, matching S3's idempotent DELETE.
    if existing.is_none() {
        return Ok(StatusCode::NO_CONTENT);
    }
    st.data
        .remove_object(&name, &key)
        .await
        .map_err(ApiError::internal)?;
    st.meta
        .delete_row(OBJECTS, &composite_key(&name, &key))
        .map_err(ApiError::internal)?;
    adjust_bucket_stats(&st, &name, -1, -(existing.unwrap().size as i64)).ok();
    st.meta.audit(
        &user.user.username,
        "console.delete",
        &format!("{name}/{key}"),
        "",
        "",
    );
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct DownloadQuery {
    pub key: String,
    /// `1` forces a download; otherwise the browser may render inline.
    #[serde(default)]
    pub download: Option<u8>,
}

/// `GET /api/buckets/{name}/object?key=...&download=1`
///
/// Used by the console's preview/download action. The same hardening headers as
/// the S3 GET path apply, so a stored HTML file cannot execute on the console
/// origin.
pub async fn download_object(
    State(st): State<AppState>,
    jar: axum_extra::extract::cookie::CookieJar,
    Path(name): Path<String>,
    Query(q): Query<DownloadQuery>,
) -> Result<Response, ApiError> {
    session::require(&st.meta, &jar)?;
    let meta: ObjectMeta = st
        .meta
        .get_json(OBJECTS, &composite_key(&name, &q.key))
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("Object not found."))?;
    let path = st.data.object_path(&name, &q.key);
    let bytes = tokio::fs::read(&path)
        .await
        .map_err(|_| ApiError::not_found("Object body missing on disk."))?;

    let filename = q.key.rsplit('/').next().unwrap_or("object");
    let disposition = if q.download.unwrap_or(0) == 1 {
        format!("attachment; filename=\"{}\"", sanitize_filename(filename))
    } else {
        format!("inline; filename=\"{}\"", sanitize_filename(filename))
    };
    let mut res = Response::new(Body::from(bytes));
    *res.status_mut() = StatusCode::OK;
    let h = res.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_str(&meta.content_type)
            .unwrap_or(HeaderValue::from_static("application/octet-stream")),
    );
    if let Ok(v) = header::HeaderValue::from_str(&disposition) {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    h.insert(
        header::CONTENT_LENGTH,
        header::HeaderValue::from_str(&meta.size.to_string())
            .unwrap_or_else(|_| header::HeaderValue::from_static("0")),
    );
    // Same defence as the S3 download path: never let stored content run.
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        header::HeaderValue::from_static("nosniff"),
    );
    if let Ok(v) = header::HeaderValue::from_str("sandbox") {
        h.insert(header::CONTENT_SECURITY_POLICY, v);
    }
    Ok(res)
}

/// Turn a request body into an `AsyncRead`, mapping transport errors to io.
fn body_reader(req: axum::http::Request<Body>) -> impl tokio::io::AsyncRead + Unpin {
    use futures_util::TryStreamExt;
    let stream = req
        .into_body()
        .into_data_stream()
        .map_err(std::io::Error::other);
    tokio_util::io::StreamReader::new(stream)
}

/// Strip anything that could break the quoted `Content-Disposition` value.
fn sanitize_filename(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            '"' | '\\' | '\r' | '\n' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect()
}

/// `GET /api/settings/limits` — read the current upload limits.
pub async fn get_limits(
    State(st): State<AppState>,
    jar: axum_extra::extract::cookie::CookieJar,
) -> Result<Json<crate::meta::Limits>, ApiError> {
    session::require(&st.meta, &jar)?;
    Ok(Json(st.limits()))
}

#[derive(Deserialize)]
pub struct LimitsPatch {
    /// `null` clears the limit (unlimited). Omitted fields are left unchanged.
    #[serde(default, deserialize_with = "double_option")]
    pub max_object_bytes: Option<Option<u64>>,
    #[serde(default)]
    pub part_bytes: Option<u64>,
}

/// Distinguish "absent" from an explicit `null`.
fn double_option<'de, D>(de: D) -> Result<Option<Option<u64>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<u64>::deserialize(de).map(Some)
}

/// `PATCH /api/settings/limits` — change upload limits, effective immediately.
pub async fn set_limits(
    State(st): State<AppState>,
    jar: axum_extra::extract::cookie::CookieJar,
    Json(body): Json<LimitsPatch>,
) -> Result<Json<crate::meta::Limits>, ApiError> {
    let user = session::require(&st.meta, &jar)?;
    let mut next = st.limits();
    if let Some(v) = body.max_object_bytes {
        // Reject nonsense rather than silently accepting a zero-byte limit that
        // would make every upload fail.
        if let Some(bytes) = v {
            if bytes == 0 {
                return Err(ApiError::bad_request(
                    "max_object_bytes must be greater than zero, or null for unlimited.",
                ));
            }
        }
        next.max_object_bytes = v;
    }
    if let Some(v) = body.part_bytes {
        if !(crate::meta::Limits::MIN_PART_BYTES..=crate::meta::Limits::MAX_PART_BYTES).contains(&v)
        {
            return Err(ApiError::bad_request(format!(
                "part_bytes must be between {} and {} bytes.",
                crate::meta::Limits::MIN_PART_BYTES,
                crate::meta::Limits::MAX_PART_BYTES
            )));
        }
        next.part_bytes = v;
    }
    let saved = st.set_limits(next).map_err(ApiError::internal)?;
    st.meta.audit(
        &user.user.username,
        "admin.limits",
        "settings",
        &format!(
            "max_object_bytes={:?} part_bytes={}",
            saved.max_object_bytes, saved.part_bytes
        ),
        "",
    );
    Ok(Json(saved))
}

/// Expose bucket existence to the upload UI without a second request.
pub fn bucket_exists(st: &AppState, name: &str) -> bool {
    matches!(st.meta.get_json::<BucketMeta>(BUCKETS, name), Ok(Some(_)))
}

#[allow(dead_code)]
fn _typecheck(_: &ObjectMeta, _: i64) {
    let _ = now_rfc3339();
}
