//! Management API for the embedded console (`/api/*`).
//!
//! Authentication is deliberately separate from the S3 data plane: a cookie
//! session here never grants object access, and an S3 credential never grants
//! console access. See `docs/SECURITY.md`.

use axum::Json;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde_json::json;

use crate::state::AppState;

/// `GET /api/health` — public liveness probe.
pub async fn health() -> impl IntoResponse {
    (
        StatusCode::OK,
        Json(json!({
            "status": "ok",
            "service": crate::config::PRODUCT,
            "version": env!("CARGO_PKG_VERSION"),
        })),
    )
}

/// `GET /api/info` — public branding/behavior flags the login page needs.
pub async fn info(axum::extract::State(st): axum::extract::State<AppState>) -> impl IntoResponse {
    Json(json!({
        "product": crate::config::PRODUCT,
        "version": env!("CARGO_PKG_VERSION"),
        "region": st.args.region,
        "public_url": st.args.public_url,
        "secure_cookies": st.args.secure_cookies(),
        "tls_mode": format!("{:?}", st.args.tls).to_lowercase(),
    }))
}
