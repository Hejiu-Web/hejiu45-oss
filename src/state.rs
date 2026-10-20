//! Shared application state and small HTTP plumbing helpers.

use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, Request};
use axum::middleware::Next;
use axum::response::Response;

use crate::auth::Authenticator;
use crate::config::Args;
use crate::data::Data;
use crate::meta::Meta;

/// Extra per-request data injected by [`capture_raw_path`].
///
/// SigV4 signs the **on-the-wire** path; by the time axum's router has matched
/// `/{bucket}/{*key}`, the path is percent-decoded. Re-encoding it cannot be
/// guaranteed to reproduce the client's exact bytes, so the raw request target
/// is captured before routing and carried as an extension.
#[derive(Debug, Clone)]
pub struct RawPath {
    pub path: String,
    pub query: String,
}

/// Everything the HTTP handlers need.
#[derive(Clone)]
pub struct AppState {
    pub args: Arc<Args>,
    pub meta: Meta,
    pub data: Data,
    pub auth: Authenticator,
    /// Server start time, for uptime reporting.
    pub started_at: std::time::Instant,
    pub started_unix: i64,
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState")
            .field("data_dir", &self.args.data_dir)
            .field("region", &self.args.region)
            .finish()
    }
}

impl AppState {
    pub fn region(&self) -> &str {
        &self.args.region
    }

    /// Best-effort client address for audit logs.
    pub fn client_addr(&self, headers: &HeaderMap) -> String {
        if self.args.trust_forwarded_for {
            if let Some(v) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
                if let Some(first) = v.split(',').next() {
                    return first.trim().to_string();
                }
            }
        }
        "proxy".to_string()
    }
}

/// Middleware: stash the raw path and query so SigV4 can use the exact bytes.
pub async fn capture_raw_path(mut req: Request<axum::body::Body>, next: Next) -> Response {
    let raw = RawPath {
        path: req.uri().path().to_string(),
        query: req.uri().query().unwrap_or("").to_string(),
    };
    req.extensions_mut().insert(raw);
    next.run(req).await
}

/// Middleware: add baseline hardening headers to every response.
pub async fn security_headers(req: Request<axum::body::Body>, next: Next) -> Response {
    let mut res = next.run(req).await;
    let h = res.headers_mut();
    h.entry("x-content-type-options")
        .or_insert(HeaderValue::from_static("nosniff"));
    h.entry("x-frame-options")
        .or_insert(HeaderValue::from_static("DENY"));
    h.entry("referrer-policy")
        .or_insert(HeaderValue::from_static("strict-origin-when-cross-origin"));
    h.entry("x-xss-protection")
        .or_insert(HeaderValue::from_static("0"));
    res
}

/// Convenience extractor alias.
pub type St = State<AppState>;

/// Current time as RFC 3339 UTC.
pub fn now_rfc3339() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

/// Unused re-export guard used by unit tests elsewhere.
#[allow(dead_code)]
fn _typecheck(_: &(crate::meta::Meta, Data)) {}
