//! Shared application state and small HTTP plumbing helpers.

use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, Request};
use axum::middleware::Next;
use axum::response::Response;

use crate::auth::Authenticator;
use crate::config::Args;
use crate::data::Data;
use crate::meta::{Limits, Meta, SETTINGS};

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
    /// Live upload limits.
    ///
    /// Cached in memory because every upload checks it, and re-read on change
    /// from the console. The database is the source of truth; this is the hot
    /// copy so a PUT does not hit redb just to read a number.
    pub limits: Arc<parking_lot::RwLock<Limits>>,
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

    /// Current upload limits (cheap read of the in-memory copy).
    pub fn limits(&self) -> Limits {
        self.limits.read().clone()
    }

    /// Persist new limits and make them take effect immediately.
    ///
    /// Write-then-cache so a failed database write does not leave the running
    /// process enforcing something that would be lost on restart.
    pub fn set_limits(&self, next: Limits) -> anyhow::Result<Limits> {
        let normalized = next.normalized();
        self.meta.put_json(SETTINGS, Limits::KEY, &normalized)?;
        *self.limits.write() = normalized.clone();
        Ok(normalized)
    }

    /// Load limits at boot.
    ///
    /// Precedence: an explicit command-line/env value wins (so a bad stored
    /// value can always be overridden by an operator with shell access), then
    /// the persisted value, then the built-in default.
    pub fn load_limits(meta: &Meta, args: &Args) -> Limits {
        let stored = meta
            .get_json::<Limits>(SETTINGS, Limits::KEY)
            .ok()
            .flatten()
            .unwrap_or_default();
        let mut limits = stored.normalized();
        if let Some(v) = args.max_object_bytes {
            limits.max_object_bytes = Some(v);
        }
        limits
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
