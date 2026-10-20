//! Management API for the embedded console (`/api/*`).
//!
//! Authentication here is a **cookie session**, entirely separate from the
//! S3 data plane's SigV4. See [`session`] for the cookie policy and
//! `docs/SECURITY.md` for the rationale.

pub mod api;
pub mod session;
pub mod upload;

pub use api::*;

use axum::Router;
use axum::routing::{delete, get, patch, post, put};

use crate::state::AppState;

/// Console API routes. Every handler except `health` / `info` / `login`
/// requires a valid session; they enforce that themselves through
/// [`session::require`].
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/health", get(api::health))
        .route("/api/info", get(api::info))
        .route("/api/auth/login", post(api::login))
        .route("/api/auth/logout", post(api::logout))
        .route("/api/auth/me", get(api::me))
        .route("/api/auth/password", post(api::change_password))
        .route("/api/overview", get(api::overview))
        .route("/api/audit", get(api::audit_log))
        .route(
            "/api/buckets",
            get(api::list_buckets).post(api::create_bucket),
        )
        .route(
            "/api/buckets/{name}",
            patch(api::update_bucket).delete(api::delete_bucket),
        )
        .route("/api/buckets/{name}/objects", get(api::list_objects))
        .route(
            "/api/buckets/{name}/objects/presign",
            post(api::presign_object),
        )
        .route("/api/keys", get(api::list_keys).post(api::create_key))
        .route(
            "/api/keys/{id}",
            delete(api::delete_key).patch(api::toggle_key),
        )
        .route("/api/keys/{id}/rotate", post(api::rotate_key))
        .route("/api/keys/{id}/grants", put(api::set_grants))
        // Console-originated data plane: uploads, downloads and deletes that a
        // signed-in operator performs from the browser (no access key needed).
        .route(
            "/api/buckets/{name}/objects",
            axum::routing::put(upload::put_object).delete(upload::delete_object),
        )
        .route(
            "/api/buckets/{name}/object",
            axum::routing::get(upload::download_object),
        )
        .route(
            "/api/buckets/{name}/uploads",
            axum::routing::post(upload::init_upload),
        )
        .route(
            "/api/buckets/{name}/uploads/{id}/parts/{part}",
            axum::routing::put(upload::put_part),
        )
        .route(
            "/api/buckets/{name}/uploads/{id}/complete",
            axum::routing::post(upload::complete_upload),
        )
        .route(
            "/api/buckets/{name}/uploads/{id}",
            axum::routing::delete(upload::abort_upload),
        )
        .route(
            "/api/settings/limits",
            axum::routing::get(upload::get_limits).patch(upload::set_limits),
        )
}
