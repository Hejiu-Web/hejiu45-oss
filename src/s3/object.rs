//! S3 object operations: PUT, GET, HEAD, DELETE, COPY, and multipart parts.
//!
//! All handlers assume the caller already verified the signature and passed
//! [`crate::s3::handlers::S3Request::authenticate`].

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use futures_util::TryStreamExt;
use tokio::io::AsyncWriteExt;

use crate::data::{StoredObject, validate_object_key};
use crate::error::{S3Error, xml_escape};
use crate::meta::{BucketPerms, MULTIPARTS, MultipartUpload, OBJECTS, ObjectMeta, composite_key};
use crate::s3::chunked::BodyEncoding;
use crate::s3::handlers::{
    S3Request, adjust_bucket_stats, collect_stored_headers, collect_user_meta,
    delete_single_object, etag_quoted, load_bucket, load_object,
};
use crate::s3::query::ByteRange;
use crate::s3::xml::{self, XmlReader};
use crate::state::{AppState, now_rfc3339};

/// Upper bound for one multipart part (S3 allows 5 GiB; we cap far lower, which
/// is still ample for a single-binary server, and document it).
pub const MAX_PART_BYTES: u64 = 512 * 1024 * 1024;
/// S3 requires every part except the last to be at least 5 MiB.
pub const MIN_PART_BYTES: u64 = 5 * 1024 * 1024;

/// Dispatch `/{bucket}/{*key}`.
pub async fn object_dispatch(
    State(st): State<AppState>,
    Path((bucket, key)): Path<(String, String)>,
    req: Request,
) -> Response {
    match dispatch_inner(&st, &bucket, &key, req).await {
        Ok(r) => r,
        Err(e) => e.into_response(),
    }
}

async fn dispatch_inner(
    st: &AppState,
    bucket: &str,
    key: &str,
    req: Request,
) -> Result<Response, S3Error> {
    let key = key.trim_start_matches('/').to_string();
    validate_object_key(&key)?;
    let method = req.method().clone();
    let s3 = S3Request::authenticate(st, &req)?;
    let sub = s3.query.subresource().map(str::to_string);
    // Computed up front: a match guard borrowing `s3.query` while `s3` is
    // destructured would be a borrow-after-move hazard.
    let has_upload_id = s3.query.value("uploadId").is_some();

    match (method, sub.as_deref()) {
        (Method::GET, None) => get_object(st, &s3, bucket, &key, req.headers()).await,
        (Method::HEAD, None) => head_object(st, &s3, bucket, &key),
        (Method::PUT, _) if has_upload_id => upload_part(st, &s3, bucket, &key, req).await,
        (Method::PUT, _) => put_object(st, &s3, bucket, &key, req).await,
        // Standard S3: CreateMultipartUpload is POST /{bucket}/{key}?uploads.
        (Method::POST, Some("uploads")) => create_multipart_object(st, &s3, bucket, &key).await,
        (Method::POST, _) if has_upload_id => complete_multipart(st, &s3, bucket, &key, req).await,
        (Method::GET, Some("uploads")) => list_multipart_uploads(st, &s3, bucket),
        (Method::DELETE, _) if has_upload_id => abort_multipart(st, &s3, bucket, &key).await,
        (Method::DELETE, None) => delete_object(st, &s3, bucket, &key).await,
        (m, sub) => Err(S3Error::not_implemented(
            &sub.map(|s| format!("{} {s}", m.as_str()))
                .unwrap_or_else(|| m.to_string()),
        )),
    }
}

async fn put_object(
    st: &AppState,
    s3: &S3Request,
    bucket: &str,
    key: &str,
    req: Request,
) -> Result<Response, S3Error> {
    let b = load_bucket(st, bucket)?;
    st.auth.authorize(s3.principal()?, &b, BucketPerms::WRITE)?;

    if let Some(copy_source) = req
        .headers()
        .get("x-amz-copy-source")
        .and_then(|v| v.to_str().ok())
    {
        return copy_object(st, s3, bucket, key, copy_source).await;
    }

    let expected = s3.expected_body_sha256().map(str::to_string);
    let stream = req
        .into_body()
        .into_data_stream()
        .map_err(std::io::Error::other);
    let raw = tokio_util::io::StreamReader::new(stream);
    // AWS SDKs stream uploads as `aws-chunked`; the framing must be stripped or
    // the stored object gains 54 bytes per part.
    let limit = st.args.max_object_bytes;
    let stored = match s3.body_encoding() {
        BodyEncoding::Identity => match expected.as_deref() {
            Some(want) => st.data.stage_verified(raw, limit, want).await?,
            None => st.data.stage(raw, limit).await?,
        },
        BodyEncoding::AwsChunked => {
            // The frames only make sense in order, so decode them first; the
            // resulting payload is then staged like any other body.
            let payload = crate::s3::chunked::Decoder::new(raw, s3.decoded_content_length())
                .read_all(limit)
                .await
                .map_err(|e| S3Error::invalid_request(format!("Invalid aws-chunked body: {e}")))?;
            match expected.as_deref() {
                Some(want) => st.data.stage_verified(&payload[..], limit, want).await?,
                None => st.data.stage(&payload[..], limit).await?,
            }
        }
    };

    let meta = build_object_meta(key, bucket, &stored, req_headers_after(st, s3));
    finalize_put(st, bucket, &meta, &stored.staged).await?;

    let mut res = Response::new(Body::empty());
    *res.status_mut() = StatusCode::OK;
    res.headers_mut().insert(
        header::ETAG,
        HeaderValue::from_str(&etag_quoted(&meta.md5_hex))
            .unwrap_or_else(|_| HeaderValue::from_static("\"\"")),
    );
    for (k, v) in s3.response_headers().into_iter() {
        if let (Ok(n), Ok(val)) = (
            header::HeaderName::from_bytes(k.as_bytes()),
            HeaderValue::from_str(&v),
        ) {
            res.headers_mut().insert(n, val);
        }
    }
    Ok(res)
}

/// Headers carried into the metadata record from the request.
///
/// `S3Request` keeps the header map; this indirection exists so the PUT path
/// can construct metadata without re-reading the consumed `Request`.
fn req_headers_after<'a>(_st: &AppState, s3: &'a S3Request) -> &'a HeaderMap {
    &s3.headers
}

fn build_object_meta(
    key: &str,
    bucket: &str,
    stored: &StoredObject,
    headers: &HeaderMap,
) -> ObjectMeta {
    // A client Content-Type wins unless it is one of the generic values that
    // tools send regardless of the payload (`curl --data-binary` sends
    // application/x-www-form-urlencoded for every body), in which case the
    // magic-number sniff is the better answer.
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
        .filter(|s| {
            !s.is_empty()
                && !s.starts_with("binary/octet-stream")
                && s != "application/x-www-form-urlencoded"
                && s != "multipart/form-data"
        })
        .unwrap_or_else(|| stored.sniffed_content_type.to_string());
    ObjectMeta {
        key: key.to_string(),
        bucket: bucket.to_string(),
        size: stored.size,
        etag: stored.sha256.clone(),
        md5_hex: stored.md5_hex.clone(),
        content_type,
        user_meta: collect_user_meta(headers),
        headers: collect_stored_headers(headers),
        last_modified: now_rfc3339(),
        multipart: false,
    }
}

/// Commit the staged body then the metadata row, and keep bucket stats right.
async fn finalize_put(
    st: &AppState,
    bucket: &str,
    meta: &ObjectMeta,
    staged: &std::path::Path,
) -> Result<(), S3Error> {
    let previous: Option<ObjectMeta> = st
        .meta
        .get_json(OBJECTS, &composite_key(bucket, &meta.key))
        .map_err(S3Error::internal)?;
    st.data
        .commit(staged, bucket, &meta.key)
        .await
        .map_err(S3Error::internal)?;
    st.meta
        .put_json(OBJECTS, &composite_key(bucket, &meta.key), meta)
        .map_err(S3Error::internal)?;
    match previous {
        Some(p) => adjust_bucket_stats(st, bucket, 0, meta.size as i64 - p.size as i64).ok(),
        None => adjust_bucket_stats(st, bucket, 1, meta.size as i64).ok(),
    };
    Ok(())
}

fn apply_object_headers(res: &mut Response, meta: &ObjectMeta) {
    let h = res.headers_mut();
    let put = |h: &mut HeaderMap, name: &'static str, value: &str| {
        if let (Ok(n), Ok(v)) = (
            header::HeaderName::from_bytes(name.as_bytes()),
            HeaderValue::from_str(value),
        ) {
            h.insert(n, v);
        }
    };
    put(h, "etag", &etag_quoted(&meta.md5_hex));
    put(h, "last-modified", &http_date(&meta.last_modified));
    put(h, "content-type", &meta.content_type);
    put(h, "accept-ranges", "bytes");
    for (k, v) in &meta.headers {
        put(h, Box::leak(k.clone().into_boxed_str()), v);
    }
    for (k, v) in &meta.user_meta {
        let name = format!("x-amz-meta-{k}");
        put(h, Box::leak(name.into_boxed_str()), v);
    }
}

/// RFC 1123 date for HTTP headers.
fn http_date(rfc3339: &str) -> String {
    time::OffsetDateTime::parse(rfc3339, &time::format_description::well_known::Rfc3339)
        .ok()
        .map(|dt| {
            dt.format(&time::macros::format_description!(
                "[weekday repr:short], [day] [month repr:short] [year] [hour]:[minute]:[second] GMT"
            ))
            .unwrap_or_else(|_| rfc3339.to_string())
        })
        .unwrap_or_else(|| rfc3339.to_string())
}

async fn get_object(
    st: &AppState,
    s3: &S3Request,
    bucket: &str,
    key: &str,
    headers: &HeaderMap,
) -> Result<Response, S3Error> {
    let b = load_bucket(st, bucket)?;
    // Public-read buckets allow anonymous GET/HEAD; everything else needs the
    // key to hold read permission.
    if !b.is_public_read() {
        st.auth.authorize(s3.principal()?, &b, BucketPerms::READ)?;
    }
    let meta = load_object(st, bucket, key)?;
    let range = headers.get(header::RANGE).and_then(|v| v.to_str().ok());

    let (start, len, status) = match range.map(|r| ByteRange::parse(r, meta.size)) {
        Some(Ok(Some(Ok(r)))) => {
            if r.start == 0 && r.end + 1 >= meta.size {
                (0, meta.size, StatusCode::OK)
            } else {
                (r.start, r.len(), StatusCode::PARTIAL_CONTENT)
            }
        }
        Some(Ok(Some(Err(_)))) | Some(Err(_)) => {
            let mut res =
                S3Error::invalid_argument("Range", range.unwrap_or_default()).into_response();
            *res.status_mut() = StatusCode::RANGE_NOT_SATISFIABLE;
            if let Ok(v) = HeaderValue::from_str(&format!("bytes */{}", meta.size)) {
                res.headers_mut().insert(header::CONTENT_RANGE, v);
            }
            return Ok(res);
        }
        _ => (0, meta.size, StatusCode::OK),
    };

    let reader = st
        .data
        .open_range(bucket, key, start, len)
        .await
        .map_err(|_| S3Error::no_such_key(key))?;
    let stream = tokio_util::io::ReaderStream::new(reader);
    let mut res = Response::new(Body::from_stream(stream));
    *res.status_mut() = status;
    res.headers_mut()
        .insert(header::CONTENT_LENGTH, HeaderValue::from(len));
    // Downloaded objects must not be sniffed into an executable type.
    res.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    // A sandboxed CSP makes even an uploaded HTML file inert when opened.
    res.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("sandbox; default-src 'none'; style-src 'unsafe-inline'"),
    );
    if status == StatusCode::PARTIAL_CONTENT {
        if let Ok(v) = HeaderValue::from_str(&format!(
            "bytes {}-{}/{}",
            start,
            start + len - 1,
            meta.size
        )) {
            res.headers_mut().insert(header::CONTENT_RANGE, v);
        }
    }
    apply_object_headers(&mut res, &meta);
    for (k, v) in s3.response_headers().into_iter() {
        if let (Ok(n), Ok(val)) = (
            header::HeaderName::from_bytes(k.as_bytes()),
            HeaderValue::from_str(&v),
        ) {
            res.headers_mut().insert(n, val);
        }
    }
    Ok(res)
}

fn head_object(
    st: &AppState,
    s3: &S3Request,
    bucket: &str,
    key: &str,
) -> Result<Response, S3Error> {
    let b = load_bucket(st, bucket)?;
    if !b.is_public_read() {
        st.auth.authorize(s3.principal()?, &b, BucketPerms::READ)?;
    }
    let meta = load_object(st, bucket, key)?;
    let mut res = Response::new(Body::empty());
    res.headers_mut()
        .insert(header::CONTENT_LENGTH, HeaderValue::from(meta.size));
    res.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    apply_object_headers(&mut res, &meta);
    Ok(res)
}

async fn delete_object(
    st: &AppState,
    s3: &S3Request,
    bucket: &str,
    key: &str,
) -> Result<Response, S3Error> {
    let b = load_bucket(st, bucket)?;
    st.auth.authorize(s3.principal()?, &b, BucketPerms::WRITE)?;
    // S3 DELETE is idempotent: deleting a missing key is a success.
    delete_single_object(st, bucket, key).await?;
    let mut res = Response::new(Body::empty());
    *res.status_mut() = StatusCode::NO_CONTENT;
    Ok(res)
}

async fn copy_object(
    st: &AppState,
    s3: &S3Request,
    dest_bucket: &str,
    dest_key: &str,
    copy_source: &str,
) -> Result<Response, S3Error> {
    let (src_bucket, src_key) = parse_copy_source(copy_source)?;
    let src = load_bucket(st, src_bucket)?;
    st.auth
        .authorize(s3.principal()?, &src, BucketPerms::READ)?;
    let dst = load_bucket(st, dest_bucket)?;
    st.auth
        .authorize(s3.principal()?, &dst, BucketPerms::WRITE)?;
    let src_meta = load_object(st, src_bucket, src_key)?;

    let mut data = Vec::new();
    let mut file = st
        .data
        .open_range(src_bucket, src_key, 0, src_meta.size)
        .await
        .map_err(|_| S3Error::no_such_key(src_key))?;
    use tokio::io::AsyncReadExt;
    file.read_to_end(&mut data)
        .await
        .map_err(S3Error::internal)?;
    let stored = st.data.stage(&data[..], None).await?;

    let mut meta = src_meta.clone();
    meta.key = dest_key.to_string();
    meta.bucket = dest_bucket.to_string();
    meta.last_modified = now_rfc3339();
    meta.size = stored.size;
    meta.etag = stored.sha256.clone();
    meta.md5_hex = stored.md5_hex.clone();
    finalize_put(st, dest_bucket, &meta, &stored.staged).await?;

    let inner = format!(
        "<LastModified>{}</LastModified><ETag>{}</ETag>",
        xml_escape(&meta.last_modified),
        xml_escape(&etag_quoted(&meta.md5_hex))
    );
    let mut res = Response::new(Body::from(xml::envelope("CopyObjectResult", &inner)));
    *res.status_mut() = StatusCode::OK;
    res.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/xml"),
    );
    Ok(res)
}

/// `x-amz-copy-source: /bucket/key` or `bucket/key`, percent-encoded.
fn parse_copy_source(raw: &str) -> Result<(&str, &str), S3Error> {
    let trimmed = raw.trim_start_matches('/');
    let decoded = crate::auth::sigv4::percent_decode(trimmed);
    // The decoded string is owned; leak it into the request lifetime (bounded
    // by request handling) so the borrow is simple.
    let leaked: &'static str = Box::leak(decoded.into_boxed_str());
    let (bucket, key) = leaked
        .split_once('/')
        .ok_or_else(|| S3Error::invalid_argument("x-amz-copy-source", raw))?;
    if bucket.is_empty() || key.is_empty() {
        return Err(S3Error::invalid_argument("x-amz-copy-source", raw));
    }
    Ok((bucket, key))
}

// ---------------------------------------------------------------------------
// Multipart upload
// ---------------------------------------------------------------------------

fn upload_key(bucket: &str, upload_id: &str) -> String {
    format!("{bucket}\u{0}{upload_id}")
}

fn load_upload(
    st: &AppState,
    bucket: &str,
    key: &str,
    upload_id: &str,
) -> Result<MultipartUpload, S3Error> {
    let up: MultipartUpload = st
        .meta
        .get_json(MULTIPARTS, &upload_key(bucket, upload_id))
        .map_err(S3Error::internal)?
        .ok_or_else(|| {
            S3Error::new(
                StatusCode::NOT_FOUND,
                "NoSuchUpload",
                "The specified upload does not exist.",
            )
        })?;
    if up.key != key {
        return Err(S3Error::new(
            StatusCode::NOT_FOUND,
            "NoSuchUpload",
            "The specified upload does not match the key.",
        ));
    }
    Ok(up)
}

/// `POST /{bucket}/{key}?uploads` — CreateMultipartUpload.
///
/// This is the form every real SDK uses (the key is in the path). The
/// bucket-level `POST /{bucket}?uploads&key=...` form is accepted as well, for
/// hand-rolled clients.
async fn create_multipart_object(
    st: &AppState,
    s3: &S3Request,
    bucket: &str,
    key: &str,
) -> Result<Response, S3Error> {
    let b = load_bucket(st, bucket)?;
    st.auth.authorize(s3.principal()?, &b, BucketPerms::WRITE)?;
    let upload_id = format!("{}{}", uuid::Uuid::new_v4().simple(), st.started_unix);
    let record = MultipartUpload {
        upload_id: upload_id.clone(),
        bucket: bucket.to_string(),
        key: key.to_string(),
        initiated_at: time::OffsetDateTime::now_utc().unix_timestamp(),
        content_type: s3
            .headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string(),
        user_meta: collect_user_meta(&s3.headers),
        headers: collect_stored_headers(&s3.headers),
        parts: Default::default(),
    };
    st.meta
        .put_json(MULTIPARTS, &upload_key(bucket, &upload_id), &record)
        .map_err(S3Error::internal)?;
    let inner = format!(
        "<Bucket>{}</Bucket><Key>{}</Key><UploadId>{}</UploadId>",
        xml_escape(bucket),
        xml_escape(key),
        xml_escape(&upload_id)
    );
    let mut res = Response::new(Body::from(xml::envelope(
        "InitiateMultipartUploadResult",
        &inner,
    )));
    *res.status_mut() = StatusCode::OK;
    res.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/xml"),
    );
    Ok(res)
}

async fn upload_part(
    st: &AppState,
    s3: &S3Request,
    bucket: &str,
    key: &str,
    req: Request,
) -> Result<Response, S3Error> {
    let upload_id = s3
        .query
        .value("uploadId")
        .ok_or_else(|| S3Error::invalid_argument("uploadId", ""))?
        .to_string();
    let part_number = s3
        .query
        .u32_value("partNumber")
        .map_err(|e| S3Error::invalid_argument("partNumber", &e.to_string()))?
        .ok_or_else(|| S3Error::invalid_argument("partNumber", ""))?;
    if !(1..=10_000).contains(&part_number) {
        return Err(S3Error::invalid_argument(
            "partNumber",
            &part_number.to_string(),
        ));
    }
    let b = load_bucket(st, bucket)?;
    st.auth.authorize(s3.principal()?, &b, BucketPerms::WRITE)?;
    let _up = load_upload(st, bucket, key, &upload_id)?;

    let stream = req
        .into_body()
        .into_data_stream()
        .map_err(std::io::Error::other);
    let raw = tokio_util::io::StreamReader::new(stream);
    // Multipart parts are *always* framed as `aws-chunked` by the AWS SDKs
    // (they are signed with STREAMING-AWS4-HMAC-SHA256-PAYLOAD).
    let stored = match s3.expected_body_sha256() {
        Some(want) => match s3.body_encoding() {
            BodyEncoding::Identity => {
                st.data
                    .stage_verified(raw, Some(MAX_PART_BYTES), want)
                    .await?
            }
            BodyEncoding::AwsChunked => {
                let payload = crate::s3::chunked::Decoder::new(raw, s3.decoded_content_length())
                    .read_all(Some(MAX_PART_BYTES))
                    .await
                    .map_err(|e| {
                        S3Error::invalid_request(format!("Invalid aws-chunked body: {e}"))
                    })?;
                st.data
                    .stage_verified(&payload[..], Some(MAX_PART_BYTES), want)
                    .await?
            }
        },
        None => match s3.body_encoding() {
            BodyEncoding::Identity => st.data.stage(raw, Some(MAX_PART_BYTES)).await?,
            BodyEncoding::AwsChunked => {
                let payload = crate::s3::chunked::Decoder::new(raw, s3.decoded_content_length())
                    .read_all(Some(MAX_PART_BYTES))
                    .await
                    .map_err(|e| {
                        S3Error::invalid_request(format!("Invalid aws-chunked body: {e}"))
                    })?;
                st.data.stage(&payload[..], Some(MAX_PART_BYTES)).await?
            }
        },
    };

    // Move the staged part into the upload directory under its part number.
    let dest = st.data.multipart_part_path(&upload_id, part_number);
    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(S3Error::internal)?;
    }
    crate::data::atomic_replace(&stored.staged, &dest)
        .await
        .map_err(S3Error::internal)?;

    // Atomic read-modify-write: AWS SDKs upload parts concurrently, so a
    // load-mutate-store sequence here would drop sibling part records.
    let md5 = stored.md5_hex.clone();
    st.meta
        .update_json::<MultipartUpload, _>(MULTIPARTS, &upload_key(bucket, &upload_id), |up| {
            up.parts.insert(part_number, md5);
        })
        .map_err(S3Error::internal)?
        .ok_or_else(|| {
            S3Error::new(
                StatusCode::NOT_FOUND,
                "NoSuchUpload",
                "The upload was aborted concurrently.",
            )
        })?;

    let mut res = Response::new(Body::empty());
    res.headers_mut().insert(
        header::ETAG,
        HeaderValue::from_str(&etag_quoted(&stored.md5_hex))
            .unwrap_or_else(|_| HeaderValue::from_static("\"\"")),
    );
    Ok(res)
}

async fn complete_multipart(
    st: &AppState,
    s3: &S3Request,
    bucket: &str,
    key: &str,
    req: Request,
) -> Result<Response, S3Error> {
    let upload_id = s3
        .query
        .value("uploadId")
        .ok_or_else(|| S3Error::invalid_argument("uploadId", ""))?
        .to_string();
    let b = load_bucket(st, bucket)?;
    st.auth.authorize(s3.principal()?, &b, BucketPerms::WRITE)?;
    let up = load_upload(st, bucket, key, &upload_id)?;

    let body = axum::body::to_bytes(req.into_body(), 1024 * 1024)
        .await
        .map_err(|_| S3Error::invalid_request("CompleteMultipartUpload body unreadable"))?;
    let doc = XmlReader::new(&String::from_utf8_lossy(&body))
        .parse()
        .map_err(S3Error::malformed_xml)?;
    let requested: Vec<u32> = doc
        .children
        .iter()
        .filter(|c| c.name == "Part")
        .filter_map(|p| {
            XmlReader::child_text(p, "PartNumber").and_then(|n| n.trim().parse::<u32>().ok())
        })
        .collect();
    if requested.is_empty() {
        return Err(S3Error::malformed_xml(
            "CompleteMultipartUpload requires at least one <Part>.",
        ));
    }
    // Every requested part must exist, in ascending order (S3 validates this).
    let mut prev = 0u32;
    for n in &requested {
        if *n <= prev {
            return Err(S3Error::invalid_request(
                "Part numbers must be in ascending order.",
            ));
        }
        prev = *n;
        if !up.parts.contains_key(n) {
            return Err(S3Error::invalid_request(format!(
                "Part {n} was not uploaded."
            )));
        }
    }

    // Concatenate parts in order into a staged object.
    let staged = st.data.new_tmp_path();
    let mut out = tokio::fs::File::create(&staged)
        .await
        .map_err(S3Error::internal)?;
    let mut total = 0u64;
    for n in &requested {
        let part_path = st.data.multipart_part_path(&upload_id, *n);
        let mut part = tokio::fs::File::open(&part_path)
            .await
            .map_err(|_| S3Error::invalid_request(format!("Part {n} is missing on disk.")))?;

        let copied = tokio::io::copy(&mut part, &mut out)
            .await
            .map_err(S3Error::internal)?;
        total += copied;
    }
    out.flush().await.map_err(S3Error::internal)?;
    out.sync_all().await.map_err(S3Error::internal)?;
    drop(out);

    let (sha, md5) = crate::data::hash_file(&staged)
        .await
        .map_err(S3Error::internal)?;
    let stored = StoredObject {
        size: total,
        sha256: sha,
        md5_hex: md5,
        staged: staged.clone(),
        sniffed_content_type: "application/octet-stream",
    };
    let mut meta = ObjectMeta {
        key: key.to_string(),
        bucket: bucket.to_string(),
        size: stored.size,
        etag: stored.sha256.clone(),
        md5_hex: stored.md5_hex.clone(),
        content_type: if up.content_type.is_empty() {
            "application/octet-stream".into()
        } else {
            up.content_type.clone()
        },
        user_meta: up.user_meta.clone(),
        headers: up.headers.clone(),
        last_modified: now_rfc3339(),
        multipart: true,
    };
    meta.last_modified = now_rfc3339();
    finalize_put(st, bucket, &meta, &stored.staged).await?;

    // Clean up the upload: metadata row then part files.
    let _ = st
        .meta
        .delete_row(MULTIPARTS, &upload_key(bucket, &upload_id));
    let _ = tokio::fs::remove_dir_all(st.data.multipart_dir(&upload_id)).await;

    let inner = format!(
        "<Location>/{}/{}</Location><Bucket>{}</Bucket><Key>{}</Key><ETag>{}</ETag>",
        xml_escape(bucket),
        xml_escape(key),
        xml_escape(bucket),
        xml_escape(key),
        xml_escape(&etag_quoted(&meta.md5_hex))
    );
    let mut res = Response::new(Body::from(xml::envelope(
        "CompleteMultipartUploadResult",
        &inner,
    )));
    *res.status_mut() = StatusCode::OK;
    res.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/xml"),
    );
    Ok(res)
}

async fn abort_multipart(
    st: &AppState,
    s3: &S3Request,
    bucket: &str,
    key: &str,
) -> Result<Response, S3Error> {
    let upload_id = s3
        .query
        .value("uploadId")
        .ok_or_else(|| S3Error::invalid_argument("uploadId", ""))?
        .to_string();
    let b = load_bucket(st, bucket)?;
    st.auth.authorize(s3.principal()?, &b, BucketPerms::WRITE)?;
    let _ = load_upload(st, bucket, key, &upload_id)?;
    let _ = st
        .meta
        .delete_row(MULTIPARTS, &upload_key(bucket, &upload_id));
    let dir = st.data.multipart_dir(&upload_id);
    // Best-effort removal, the row is already gone so the space is reclaimable.
    let _ = tokio::fs::remove_dir_all(dir).await;
    Ok(Response::new(Body::empty()))
}

fn list_multipart_uploads(
    st: &AppState,
    s3: &S3Request,
    bucket: &str,
) -> Result<Response, S3Error> {
    let b = load_bucket(st, bucket)?;
    st.auth.authorize(s3.principal()?, &b, BucketPerms::READ)?;
    let uploads: Vec<MultipartUpload> = st
        .meta
        .scan_json(MULTIPARTS, Some(&format!("{bucket}\u{0}")), 10_000)
        .map_err(S3Error::internal)?
        .into_iter()
        .map(|(_, u)| u)
        .collect();
    let mut inner = format!(
        "<Bucket>{}</Bucket><IsTruncated>false</IsTruncated>",
        xml_escape(bucket)
    );
    for u in &uploads {
        inner.push_str(&format!(
            "<Upload><Key>{}</Key><UploadId>{}</UploadId><Initiated>{}</Initiated></Upload>",
            xml_escape(&u.key),
            xml_escape(&u.upload_id),
            xml_escape(&format_unix(u.initiated_at))
        ));
    }
    Ok(Response::new(Body::from(xml::envelope(
        "ListMultipartUploadsResult",
        &inner,
    ))))
}

fn format_unix(ts: i64) -> String {
    time::OffsetDateTime::from_unix_timestamp(ts)
        .ok()
        .and_then(|t| {
            t.format(&time::format_description::well_known::Rfc3339)
                .ok()
        })
        .unwrap_or_default()
}

/// Re-export used by `handlers.rs`.
pub use crate::data::validate_object_key as _validate_object_key;

/// Type alias kept for documentation of ownership in handlers.
pub type ArcState = Arc<AppState>;
