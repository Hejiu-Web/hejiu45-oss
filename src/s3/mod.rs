//! S3 protocol handlers.
//!
//! Routing model (see `server.rs`):
//!
//! | Method | Path | Operation |
//! |---|---|---|
//! | GET | `/` | ListBuckets |
//! | GET | `/{bucket}?list-type=2` | ListObjectsV2 |
//! | PUT | `/{bucket}` | CreateBucket |
//! | HEAD | `/{bucket}` | HeadBucket |
//! | DELETE | `/{bucket}` | DeleteBucket |
//! | GET/HEAD/PUT/DELETE | `/{bucket}/{*key}` | Object operations |
//! | POST | `/{bucket}` | DeleteObjects (batch) |
//!
//! Every request is authenticated by SigV4 before reaching an operation;
//! per-operation permission is then checked against the bucket ACL.

use axum::body::Body;
use axum::extract::{Path, Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Serialize;

use crate::error::{S3Error, xml_escape};
use crate::meta::{BUCKETS, BucketMeta, ObjectMeta};
use crate::state::AppState;

/// Health endpoint that leaks nothing but liveness and the API version.
pub async fn health() -> impl IntoResponse {
    (
        StatusCode::OK,
        [("content-type", "application/json")],
        r#"{"status":"ok","service":"hejiu45-oss"}"#,
    )
}

/// Temporary router placeholder replaced by the full implementation.
pub async fn not_yet(State(_st): State<AppState>, req: Request) -> Response {
    let _ = (req.method(), req.headers());
    let e = S3Error::new(StatusCode::NOT_IMPLEMENTED, "NotImplemented", "Not implemented yet.");
    e.into_response()
}

/// Placeholder bucket listing (replaced below).
pub async fn list_buckets(State(st): State<AppState>) -> Response {
    let buckets: Vec<BucketMeta> = st
        .meta
        .scan_json(BUCKETS, None, 10_000)
        .unwrap_or_default()
        .into_iter()
        .map(|(_, b)| b)
        .collect();
    list_buckets_xml(&buckets, st.region(), st.region()).into_response()
}

#[derive(Serialize)]
struct _Unused;

/// Build the ListAllMyBucketsResult document.
pub fn list_buckets_xml(buckets: &[BucketMeta], owner_id: &str, owner_name: &str) -> (StatusCode, [(&'static str, &'static str); 1], String) {
    let mut body = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<ListAllMyBucketsResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">",
    );
    body.push_str(&format!(
        "<Owner><ID>{}</ID><DisplayName>{}</DisplayName></Owner><Buckets>",
        xml_escape(owner_id),
        xml_escape(owner_name)
    ));
    for b in buckets {
        body.push_str(&format!(
            "<Bucket><Name>{}</Name><CreationDate>{}</CreationDate></Bucket>",
            xml_escape(&b.name),
            xml_escape(&b.created_at)
        ));
    }
    body.push_str("</Buckets></ListAllMyBucketsResult>");
    (StatusCode::OK, [("content-type", "application/xml")], body)
}

/// Unused helper keeping imports honest until the full handlers land.
#[allow(dead_code)]
fn _touch(_: &ObjectMeta, _: &Method, _: &HeaderMap, _: &HeaderValue, _: &Path<(String, String)>, _: &Body) {}
