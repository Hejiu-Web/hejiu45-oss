//! S3 object and bucket operations.
//!
//! ## Authentication
//!
//! [`S3Request::authenticate`] is the single gate every operation passes: it
//! performs SigV4 verification, then resolves the bucket and enforces the
//! bucket ACL for the specific permission the operation needs. Nothing below
//! this line touches storage without a verified principal.
//!
//! ## Payload integrity
//!
//! When a client declares `x-amz-content-sha256` as a concrete digest, the
//! streamed body is compared against it (`stage_verified`). `UNSIGNED-PAYLOAD`
//! and the streaming-signature marker are accepted as-is for header auth, and
//! presigned URLs always use `UNSIGNED-PAYLOAD` (they carry no body).

use axum::body::Body;
use axum::extract::{Path, Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Serialize;

use crate::auth::sigv4::{AuthStyle, payload_hash_is_literal};
use crate::auth::{AuthContext, Principal};
use crate::data::validate_bucket_name;
use crate::error::{S3Error, xml_escape};
use crate::meta::{
    BUCKETS, BucketMeta, BucketPerms, MULTIPARTS, MultipartUpload, OBJECTS, ObjectMeta,
    composite_key,
};
use crate::s3::query::S3Query;
use crate::s3::xml::{self, XmlReader};
use crate::state::{AppState, RawPath, now_rfc3339};

/// Everything the handlers need about one request, resolved in one place.
pub struct S3Request {
    pub method: Method,
    pub headers: HeaderMap,
    pub query: S3Query,
    pub raw_path: String,
    pub raw_query: String,
    /// None for an anonymous request (no credential at all).
    pub principal: Option<Principal>,
}

impl S3Request {
    /// Verify the signature of an incoming request.
    pub fn authenticate(state: &AppState, req: &Request) -> Result<Self, S3Error> {
        // `nest` strips the S3 mount prefix before routing, so `req.uri()`
        // holds `/bucket/key`. SigV4 signs the full request target, which is
        // captured by `capture_raw_path` before the router rewrites it.
        let raw_path = req
            .extensions()
            .get::<RawPath>()
            .map(|r| r.path.clone())
            .unwrap_or_else(|| req.uri().path().to_string());
        let raw_query = req
            .extensions()
            .get::<RawPath>()
            .map(|r| r.query.clone())
            .unwrap_or_else(|| req.uri().query().unwrap_or("").to_string());
        let headers = req.headers().clone();
        let now = time::OffsetDateTime::now_utc().unix_timestamp();
        let ctx = AuthContext {
            method: req.method(),
            headers: &headers,
            raw_path: &raw_path,
            raw_query: &raw_query,
            content_sha256: headers
                .get("x-amz-content-sha256")
                .and_then(|v| v.to_str().ok())
                .map(str::to_string),
            now,
        };
        let principal = state.auth.verify(&ctx)?;
        Ok(Self {
            method: req.method().clone(),
            headers,
            query: S3Query::parse(&raw_query),
            raw_path,
            raw_query,
            principal,
        })
    }

    /// The verified caller, or an error for an operation that needs one.
    ///
    /// Only `GetObject`/`HeadObject` accept anonymous access (and only on a
    /// `public-read` bucket); every other operation calls this.
    pub fn principal(&self) -> Result<&Principal, S3Error> {
        self.principal
            .as_ref()
            .ok_or_else(S3Error::missing_signature)
    }

    /// How this request's body is framed (`aws-chunked` for SDK streaming
    /// uploads, plain bytes otherwise).
    pub fn body_encoding(&self) -> crate::s3::chunked::BodyEncoding {
        let enc = self
            .headers
            .get("content-encoding")
            .and_then(|v| v.to_str().ok());
        let decoded = self
            .headers
            .get("x-amz-decoded-content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok());
        crate::s3::chunked::BodyEncoding::detect(enc, decoded)
    }

    /// `x-amz-decoded-content-length`, the payload size after de-framing.
    pub fn decoded_content_length(&self) -> Option<u64> {
        self.headers
            .get("x-amz-decoded-content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
    }

    /// The payload size the client declared, preferring the de-framed length.
    ///
    /// Used to reject an oversized upload before reading it. Advisory only: the
    /// streaming limit still applies, because a client can lie here.
    pub fn declared_body_len(&self) -> Option<u64> {
        self.decoded_content_length().or_else(|| {
            self.headers
                .get(header::CONTENT_LENGTH)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok())
        })
    }

    /// Declared payload hash that must be enforced while streaming, if any.
    pub fn expected_body_sha256(&self) -> Option<&str> {
        let h = self
            .headers
            .get("x-amz-content-sha256")
            .and_then(|v| v.to_str().ok())?;
        // Presigned URLs never carry a body, so `UNSIGNED-PAYLOAD` is all they
        // can use.
        let presigned = matches!(
            self.principal.as_ref().map(|p| p.style),
            Some(AuthStyle::Presigned)
        );
        if payload_hash_is_literal(h) || presigned {
            None
        } else if h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit()) {
            Some(h)
        } else {
            None
        }
    }

    /// Standard response headers every S3 reply carries.
    pub(crate) fn response_headers(&self) -> Vec<(&'static str, String)> {
        vec![
            ("x-amz-request-id", uuid::Uuid::new_v4().to_string()),
            ("x-amz-id-2", uuid::Uuid::new_v4().to_string()),
            ("server", "hejiu45-oss".to_string()),
        ]
    }
}

fn xml_response(status: StatusCode, body: String, extra: Vec<(&'static str, String)>) -> Response {
    let mut res = Response::new(Body::from(body));
    *res.status_mut() = status;
    res.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/xml"),
    );
    for (k, v) in extra {
        if let Ok(val) = HeaderValue::from_str(&v) {
            if let Ok(name) = header::HeaderName::from_bytes(k.as_bytes()) {
                res.headers_mut().insert(name, val);
            }
        }
    }
    res
}

fn empty_response(status: StatusCode, extra: Vec<(&'static str, String)>) -> Response {
    let mut res = Response::new(Body::empty());
    *res.status_mut() = status;
    for (k, v) in extra {
        if let Ok(val) = HeaderValue::from_str(&v) {
            if let Ok(name) = header::HeaderName::from_bytes(k.as_bytes()) {
                res.headers_mut().insert(name, val);
            }
        }
    }
    res
}

pub(crate) fn load_bucket(state: &AppState, name: &str) -> Result<BucketMeta, S3Error> {
    state
        .meta
        .get_json::<BucketMeta>(BUCKETS, name)
        .map_err(S3Error::internal)?
        .ok_or_else(|| S3Error::no_such_bucket(name))
}

pub(crate) fn load_object(
    state: &AppState,
    bucket: &str,
    key: &str,
) -> Result<ObjectMeta, S3Error> {
    state
        .meta
        .get_json::<ObjectMeta>(OBJECTS, &composite_key(bucket, key))
        .map_err(S3Error::internal)?
        .ok_or_else(|| S3Error::no_such_key(key))
}

/// Quoted hex MD5, i.e. what S3 returns as `ETag` for a simple upload.
pub fn etag_quoted(md5_hex: &str) -> String {
    format!("\"{md5_hex}\"")
}

// ---------------------------------------------------------------------------
// ListBuckets
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct _Noop;

pub async fn list_buckets(State(st): State<AppState>, req: Request) -> Response {
    let s3 = match S3Request::authenticate(&st, &req) {
        Ok(r) => r,
        Err(e) => return e.into_response(),
    };
    let principal = match s3.principal() {
        Ok(p) => p,
        Err(e) => return e.into_response(),
    };
    let all: Vec<BucketMeta> = st
        .meta
        .scan_json(BUCKETS, None, 10_000)
        .unwrap_or_default()
        .into_iter()
        .map(|(_, b)| b)
        .collect();
    // A key only sees the buckets it can read — list results are themselves
    // information, so they follow the same ACL as the objects.
    let visible: Vec<&BucketMeta> = all
        .iter()
        .filter(|b| {
            b.perms
                .get(&principal.access_key)
                .map(BucketPerms::can_read)
                .unwrap_or(false)
        })
        .collect();
    let mut inner = String::new();
    inner.push_str(&format!(
        "<Owner><ID>{}</ID><DisplayName>{}</DisplayName></Owner><Buckets>",
        xml_escape(&principal.access_key),
        xml_escape(&principal.label)
    ));
    for b in visible {
        inner.push_str(&format!(
            "<Bucket><Name>{}</Name><CreationDate>{}</CreationDate></Bucket>",
            xml_escape(&b.name),
            xml_escape(&b.created_at)
        ));
    }
    inner.push_str("</Buckets>");
    xml_response(
        StatusCode::OK,
        xml::envelope("ListAllMyBucketsResult", &inner),
        s3.response_headers(),
    )
}

// ---------------------------------------------------------------------------
// Bucket operations
// ---------------------------------------------------------------------------

pub async fn bucket_dispatch(
    State(st): State<AppState>,
    Path(bucket): Path<String>,
    req: Request,
) -> Response {
    // Split the body off after authentication: SigV4 verification needs the
    // headers/URI only, and the body is streamed by each operation.
    let (parts, body) = req.into_parts();
    let head_only = Request::from_parts(parts.clone(), axum::body::Body::empty());
    match bucket_and_body(&st, &bucket, head_only, parts, body).await {
        Ok(r) => r,
        Err(e) => e.into_response(),
    }
}

async fn bucket_and_body(
    st: &AppState,
    bucket: &str,
    req: Request,
    parts: http::request::Parts,
    body: Body,
) -> Result<Response, S3Error> {
    let method = req.method().clone();
    let s3 = S3Request::authenticate(st, &req)?;
    let sub = s3.query.subresource().map(str::to_string);

    match (method, sub.as_deref()) {
        (Method::PUT, None) => create_bucket(st, &s3, bucket).await,
        (Method::HEAD, None) => head_bucket(st, &s3, bucket),
        (Method::DELETE, None) => delete_bucket(st, &s3, bucket).await,
        (Method::GET, None) if s3.query.is_list_v2() => list_objects_v2(st, &s3, bucket),
        (Method::GET, Some("location")) => {
            let b = load_bucket(st, bucket)?;
            st.auth.authorize(s3.principal()?, &b, BucketPerms::READ)?;
            let inner = format!(
                "<LocationConstraint>{}</LocationConstraint>",
                xml_escape(st.region())
            );
            Ok(xml_response(
                StatusCode::OK,
                xml::envelope("LocationConstraint", &inner),
                s3.response_headers(),
            ))
        }
        (Method::POST, Some("delete")) => delete_objects(st, &s3, bucket, body).await,
        // Multipart upload control plane.
        (Method::POST, Some("uploads")) => create_multipart(st, &s3, bucket, &parts.headers).await,
        // Unsupported sub-resources: report them instead of silently ignoring.
        (Method::GET | Method::PUT | Method::DELETE | Method::POST, Some(other)) => {
            Err(S3Error::not_implemented(other))
        }
        // `GET /{bucket}` without list-type=2 is ListObjects (v1).
        (Method::GET, None) => list_objects_v1(st, &s3, bucket),
        (m, _) => Err(S3Error::not_implemented(m.as_str())),
    }
}

async fn create_bucket(st: &AppState, s3: &S3Request, bucket: &str) -> Result<Response, S3Error> {
    validate_bucket_name(bucket)?;
    if st
        .meta
        .get_json::<BucketMeta>(BUCKETS, bucket)
        .map_err(S3Error::internal)?
        .is_some()
    {
        return Err(S3Error::bucket_already_exists(bucket));
    }
    let meta = BucketMeta {
        name: bucket.to_string(),
        created_at: now_rfc3339(),
        owner: s3.principal()?.access_key.clone(),
        // The creating key gets full access; other keys must be granted.
        perms: [(s3.principal()?.access_key.clone(), BucketPerms::FULL)].into(),
        visibility: "private".to_string(),
        object_count: 0,
        total_bytes: 0,
        custom_domain: String::new(),
    };
    st.meta
        .put_json(BUCKETS, bucket, &meta)
        .map_err(S3Error::internal)?;
    state_audit(st, s3, "bucket.create", bucket, "");
    let mut res = empty_response(StatusCode::OK, s3.response_headers());
    res.headers_mut().insert(
        header::LOCATION,
        HeaderValue::from_str(&format!("/{bucket}"))
            .unwrap_or_else(|_| HeaderValue::from_static("/")),
    );
    Ok(res)
}

fn head_bucket(st: &AppState, s3: &S3Request, bucket: &str) -> Result<Response, S3Error> {
    let b = load_bucket(st, bucket)?;
    st.auth.authorize(s3.principal()?, &b, BucketPerms::READ)?;
    Ok(empty_response(StatusCode::OK, s3.response_headers()))
}

async fn delete_bucket(st: &AppState, s3: &S3Request, bucket: &str) -> Result<Response, S3Error> {
    let b = load_bucket(st, bucket)?;
    st.auth.authorize(s3.principal()?, &b, BucketPerms::WRITE)?;
    let has_objects = !st
        .meta
        .scan_json::<ObjectMeta>(OBJECTS, Some(&format!("{bucket}\u{0}")), 1)
        .map_err(S3Error::internal)?
        .is_empty();
    if has_objects {
        return Err(S3Error::bucket_not_empty());
    }
    // Abandoned multipart uploads must not keep a bucket alive either.
    let _ = st.meta.delete_prefix(MULTIPARTS, &format!("{bucket}\u{0}"));
    st.meta
        .delete_row(BUCKETS, bucket)
        .map_err(S3Error::internal)?;
    st.data
        .remove_bucket_dir(bucket)
        .await
        .map_err(S3Error::internal)?;
    state_audit(st, s3, "bucket.delete", bucket, "");
    Ok(empty_response(
        StatusCode::NO_CONTENT,
        s3.response_headers(),
    ))
}

fn list_objects_v1(st: &AppState, s3: &S3Request, bucket: &str) -> Result<Response, S3Error> {
    list_objects_common(st, s3, bucket, false)
}

fn list_objects_v2(st: &AppState, s3: &S3Request, bucket: &str) -> Result<Response, S3Error> {
    list_objects_common(st, s3, bucket, true)
}

/// Shared ListObjects/ListObjectsV2 implementation.
///
/// `prefix` + `delimiter` implement the directory illusion; `continuation-token`
/// (v2) / `marker` (v1) implement pagination over the sorted key space.
fn list_objects_common(
    st: &AppState,
    s3: &S3Request,
    bucket: &str,
    v2: bool,
) -> Result<Response, S3Error> {
    let b = load_bucket(st, bucket)?;
    st.auth.authorize(s3.principal()?, &b, BucketPerms::READ)?;

    let prefix = s3.query.value("prefix").unwrap_or("").to_string();
    let delimiter = s3.query.value("delimiter").unwrap_or("").to_string();
    let max_keys = s3
        .query
        .u32_value("max-keys")
        .map_err(|e| S3Error::invalid_argument("max-keys", &e.to_string()))?;
    let max_keys = max_keys.unwrap_or(1000).clamp(1, 1000) as usize;
    let start_after = s3.query.value("start-after").unwrap_or("").to_string();
    let token = s3
        .query
        .value("continuation-token")
        .map(str::to_string)
        .or_else(|| s3.query.value("marker").map(str::to_string))
        .unwrap_or_default();
    let cursor = if v2 && !start_after.is_empty() && token.is_empty() {
        start_after.clone()
    } else {
        token.clone()
    };

    let all: Vec<ObjectMeta> = st
        .meta
        .scan_json::<ObjectMeta>(OBJECTS, Some(&format!("{bucket}\u{0}")), usize::MAX)
        .map_err(S3Error::internal)?
        .into_iter()
        .map(|(_, o)| o)
        .collect();
    let mut keys: Vec<&ObjectMeta> = all
        .iter()
        .filter(|o| o.key.starts_with(&prefix))
        .filter(|o| cursor.is_empty() || o.key.as_str() > cursor.as_str())
        .collect();
    keys.sort_by(|a, b| a.key.cmp(&b.key));

    let mut contents: Vec<&ObjectMeta> = Vec::new();
    let mut common: Vec<String> = Vec::new();
    let mut truncated = false;
    let mut next_token = String::new();
    for o in keys {
        let remainder = &o.key[prefix.len()..];
        if !delimiter.is_empty() {
            if let Some(pos) = remainder.find(&delimiter) {
                let cp = format!("{prefix}{}", &remainder[..pos + delimiter.len()]);
                if common.last() != Some(&cp) {
                    if contents.len() + common.len() >= max_keys {
                        truncated = true;
                        break;
                    }
                    common.push(cp);
                }
                continue;
            }
        }
        if contents.len() + common.len() >= max_keys {
            truncated = true;
            break;
        }
        contents.push(o);
    }
    if truncated {
        next_token = contents
            .last()
            .map(|o| o.key.clone())
            .or_else(|| common.last().cloned())
            .unwrap_or_default();
    }

    let mut inner = String::new();
    if v2 {
        inner.push_str(&format!("<Name>{}</Name>", xml_escape(bucket)));
        inner.push_str(&format!("<Prefix>{}</Prefix>", xml_escape(&prefix)));
        inner.push_str(&format!("<MaxKeys>{max_keys}</MaxKeys>"));
        inner.push_str(&format!(
            "<KeyCount>{}</KeyCount>",
            contents.len() + common.len()
        ));
        inner.push_str(&format!("<IsTruncated>{truncated}</IsTruncated>"));
        if !start_after.is_empty() {
            inner.push_str(&format!(
                "<StartAfter>{}</StartAfter>",
                xml_escape(&start_after)
            ));
        }
        if !token.is_empty() {
            inner.push_str(&format!(
                "<ContinuationToken>{}</ContinuationToken>",
                xml_escape(&token)
            ));
        }
    } else {
        inner.push_str(&format!("<Name>{}</Name>", xml_escape(bucket)));
        inner.push_str(&format!("<Prefix>{}</Prefix>", xml_escape(&prefix)));
        if !delimiter.is_empty() {
            inner.push_str(&format!(
                "<Delimiter>{}</Delimiter>",
                xml_escape(&delimiter)
            ));
        }
        inner.push_str(&format!("<MaxKeys>{max_keys}</MaxKeys>"));
        inner.push_str(&format!("<IsTruncated>{truncated}</IsTruncated>"));
        if !token.is_empty() {
            inner.push_str(&format!("<Marker>{}</Marker>", xml_escape(&token)));
        } else {
            inner.push_str("<Marker></Marker>");
        }
        if truncated {
            inner.push_str(&format!(
                "<NextMarker>{}</NextMarker>",
                xml_escape(&next_token)
            ));
        }
    }
    if !delimiter.is_empty() {
        inner.push_str(&format!(
            "<Delimiter>{}</Delimiter>",
            xml_escape(&delimiter)
        ));
    }
    for o in &contents {
        inner.push_str(&object_entry(o));
    }
    for cp in &common {
        inner.push_str(&format!(
            "<CommonPrefixes><Prefix>{}</Prefix></CommonPrefixes>",
            xml_escape(cp)
        ));
    }
    if v2 && truncated {
        inner.push_str(&format!(
            "<NextContinuationToken>{}</NextContinuationToken>",
            xml_escape(&next_token)
        ));
    }
    // ListObjects and ListObjectsV2 share the same root element.
    Ok(xml_response(
        StatusCode::OK,
        xml::envelope("ListBucketResult", &inner),
        s3.response_headers(),
    ))
}

fn object_entry(o: &ObjectMeta) -> String {
    format!(
        "<Contents><Key>{}</Key><LastModified>{}</LastModified><ETag>{}</ETag><Size>{}</Size><StorageClass>STANDARD</StorageClass></Contents>",
        xml_escape(&o.key),
        xml_escape(&o.last_modified),
        xml_escape(&etag_quoted(&o.md5_hex)),
        o.size
    )
}

async fn delete_objects(
    st: &AppState,
    s3: &S3Request,
    bucket: &str,
    body: Body,
) -> Result<Response, S3Error> {
    let b = load_bucket(st, bucket)?;
    st.auth.authorize(s3.principal()?, &b, BucketPerms::WRITE)?;
    let body = axum::body::to_bytes(body, 2 * 1024 * 1024)
        .await
        .map_err(|_| S3Error::invalid_request("DeleteObjects body too large or unreadable"))?;
    let text = String::from_utf8_lossy(&body);
    let doc = XmlReader::new(&text)
        .parse()
        .map_err(S3Error::malformed_xml)?;
    let quiet = XmlReader::child_text(&doc, "Quiet") == Some("true");
    let keys: Vec<String> = doc
        .children
        .iter()
        .filter(|c| c.name == "Object")
        .filter_map(|c| XmlReader::child_text(c, "Key").map(str::to_string))
        .collect();
    if keys.is_empty() {
        return Err(S3Error::malformed_xml(
            "DeleteObjects requires at least one <Object>.",
        ));
    }
    if keys.len() > 10_000 {
        return Err(S3Error::malformed_xml(
            "DeleteObjects accepts at most 10000 keys.",
        ));
    }

    let mut inner = String::new();
    let mut deleted = 0usize;
    for key in &keys {
        match delete_single_object(st, bucket, key).await {
            Ok(existed) => {
                deleted += 1;
                if !quiet {
                    inner.push_str(&format!(
                        "<Deleted><Key>{}</Key></Deleted>",
                        xml_escape(key)
                    ));
                    let _ = existed;
                }
            }
            Err(e) => inner.push_str(&format!(
                "<Error><Key>{}</Key><Code>{}</Code><Message>{}</Message></Error>",
                xml_escape(key),
                xml_escape(e.code),
                xml_escape(&e.message)
            )),
        }
    }
    state_audit(
        st,
        s3,
        "object.delete_many",
        bucket,
        &format!("{deleted} objects"),
    );
    Ok(xml_response(
        StatusCode::OK,
        xml::envelope("DeleteResult", &inner),
        s3.response_headers(),
    ))
}

/// Delete one object (metadata first, then body). Idempotent.
pub(crate) async fn delete_single_object(
    st: &AppState,
    bucket: &str,
    key: &str,
) -> Result<bool, S3Error> {
    let existed = st
        .meta
        .delete_row(OBJECTS, &composite_key(bucket, key))
        .map_err(S3Error::internal)?;
    adjust_bucket_stats(st, bucket, -1, 0).ok();
    st.data
        .remove_object(bucket, key)
        .await
        .map_err(S3Error::internal)?;
    Ok(existed)
}

pub(crate) fn adjust_bucket_stats(
    st: &AppState,
    bucket: &str,
    count_delta: i64,
    byte_delta: i64,
) -> anyhow::Result<()> {
    let Some(mut b) = st.meta.get_json::<BucketMeta>(BUCKETS, bucket)? else {
        return Ok(());
    };
    b.object_count = (b.object_count as i64 + count_delta).max(0) as u64;
    b.total_bytes = (b.total_bytes as i64 + byte_delta).max(0) as u64;
    st.meta.put_json(BUCKETS, bucket, &b)
}

fn state_audit(st: &AppState, s3: &S3Request, action: &str, target: &str, detail: &str) {
    let actor = match s3.principal.as_ref() {
        Some(p) => format!("s3:{}", p.label),
        None => "s3:anonymous".to_string(),
    };
    st.meta.audit(
        &actor,
        action,
        target,
        detail,
        s3.headers
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .unwrap_or(""),
    );
}

// ---------------------------------------------------------------------------
// Multipart upload (control plane; part handlers live in object.rs)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct UploadTarget {
    pub bucket: String,
    pub key: String,
}

async fn create_multipart(
    st: &AppState,
    s3: &S3Request,
    bucket: &str,
    headers: &HeaderMap,
) -> Result<Response, S3Error> {
    let key = s3
        .query
        .value("key")
        .ok_or_else(|| {
            S3Error::invalid_request("CreateMultipartUpload requires the `key` query parameter.")
        })?
        .to_string();
    crate::data::validate_object_key(&key)?;
    let b = load_bucket(st, bucket)?;
    st.auth.authorize(s3.principal()?, &b, BucketPerms::WRITE)?;
    let upload_id = format!("{}{}", uuid::Uuid::new_v4().simple(), st.started_unix);
    let record = MultipartUpload {
        upload_id: upload_id.clone(),
        bucket: bucket.to_string(),
        key: key.clone(),
        initiated_at: time::OffsetDateTime::now_utc().unix_timestamp(),
        content_type: headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string(),
        user_meta: collect_user_meta(headers),
        headers: collect_stored_headers(headers),
        parts: Default::default(),
    };
    st.meta
        .put_json(MULTIPARTS, &format!("{bucket}\u{0}{upload_id}"), &record)
        .map_err(S3Error::internal)?;
    let inner = format!(
        "<Bucket>{}</Bucket><Key>{}</Key><UploadId>{}</UploadId>",
        xml_escape(bucket),
        xml_escape(&key),
        xml_escape(&upload_id)
    );
    state_audit(
        st,
        s3,
        "object.multipart_init",
        &format!("{bucket}/{key}"),
        &upload_id,
    );
    Ok(xml_response(
        StatusCode::OK,
        xml::envelope("InitiateMultipartUploadResult", &inner),
        s3.response_headers(),
    ))
}

/// `x-amz-meta-*` headers without the prefix.
pub fn collect_user_meta(headers: &HeaderMap) -> std::collections::BTreeMap<String, String> {
    headers
        .iter()
        .filter_map(|(k, v)| {
            k.as_str().strip_prefix("x-amz-meta-").and_then(|name| {
                v.to_str()
                    .ok()
                    .map(|val| (name.to_string(), val.to_string()))
            })
        })
        .collect()
}

/// Content headers preserved across GET/HEAD.
pub fn collect_stored_headers(headers: &HeaderMap) -> std::collections::BTreeMap<String, String> {
    const KEEP: &[&str] = &[
        "cache-control",
        "content-disposition",
        "content-encoding",
        "content-language",
        "expires",
    ];
    headers
        .iter()
        .filter_map(|(k, v)| {
            let name = k.as_str();
            if KEEP.contains(&name) {
                v.to_str()
                    .ok()
                    .map(|val| (name.to_string(), val.to_string()))
            } else {
                None
            }
        })
        .collect()
}
