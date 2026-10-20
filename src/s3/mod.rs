//! S3 protocol surface.
//!
//! Routing model (see `server.rs`):
//!
//! | Method | Path | Operation |
//! |---|---|---|
//! | GET | `/` | ListBuckets |
//! | GET | `/{bucket}?list-type=2` | ListObjectsV2 |
//! | GET | `/{bucket}` | ListObjects (v1) |
//! | PUT | `/{bucket}` | CreateBucket |
//! | HEAD | `/{bucket}` | HeadBucket |
//! | DELETE | `/{bucket}` | DeleteBucket |
//! | POST | `/{bucket}?delete` | DeleteObjects |
//! | POST | `/{bucket}?uploads&key=` | CreateMultipartUpload |
//! | GET/HEAD/PUT/DELETE | `/{bucket}/{*key}` | Object operations |
//! | PUT | `/{bucket}/{*key}?uploadId=&partNumber=` | UploadPart |
//! | POST | `/{bucket}/{*key}?uploadId=` | CompleteMultipartUpload |
//! | DELETE | `/{bucket}/{*key}?uploadId=` | AbortMultipartUpload |
//!
//! Every request is authenticated by SigV4 before reaching an operation;
//! per-operation permission is then checked against the bucket ACL.

pub mod handlers;
pub mod object;
pub mod query;
pub mod xml;

use axum::extract::{Path, Request, State};
use axum::response::{IntoResponse, Response};

use crate::state::AppState;

pub use handlers::list_buckets;

/// `GET /healthz` — unauthenticated liveness for load balancers.
pub async fn health() -> impl IntoResponse {
    (
        axum::http::StatusCode::OK,
        [("content-type", "application/json")],
        r#"{"status":"ok","service":"hejiu45-oss"}"#,
    )
}

/// `GET /` — ListBuckets (signed).
pub async fn root(State(st): State<AppState>, req: Request) -> Response {
    list_buckets(State(st), req).await
}

/// `/{bucket}` operations.
pub async fn bucket(State(st): State<AppState>, path: Path<String>, req: Request) -> Response {
    handlers::bucket_dispatch(State(st), path, req).await
}

/// `/{bucket}/{*key}` operations.
pub async fn object(
    State(st): State<AppState>,
    path: Path<(String, String)>,
    req: Request,
) -> Response {
    object::object_dispatch(State(st), path, req).await
}
