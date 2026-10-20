//! Serving of the embedded console (Vue 3 SPA built by Vite).
//!
//! `rust-embed` writes `frontend/dist/` into the binary at compile time, so a
//! running server needs no static files and no `Node.js`.
//!
//! Caching policy:
//! * `index.html` → `no-cache` (must always see the latest asset hashes)
//! * hashed assets (`/assets/...`) → `immutable, max-age=1y` + strong ETag
//! * everything else → ETag with `must-revalidate`, 304 on match

use axum::body::Body;
use axum::http::{HeaderValue, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "frontend/dist/"]
#[include = "*.html"]
#[include = "assets/*"]
#[include = "*.ico"]
#[include = "*.png"]
#[include = "*.svg"]
#[include = "*.webmanifest"]
struct Assets;

/// Whether the embedded bundle is the build-placeholder rather than the real
/// console (surfaced by `/api/health`).
pub fn console_embedded() -> bool {
    Assets::get("assets").is_some()
        || Assets::iter().any(|p| p.starts_with("assets/") && p.ends_with(".js"))
}

/// Serve an embedded file, or the SPA shell for client-side routes.
pub async fn static_handler(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };

    if let Some(file) = Assets::get(path) {
        return serve(path, file.data, cache_control(path));
    }
    // Hash-mode routing means every real navigation hits `/`; still, fall back
    // to the shell for unknown paths so bookmarks and typos land in the app.
    match Assets::get("index.html") {
        Some(index) => serve("index.html", index.data, "no-cache"),
        None => (StatusCode::NOT_FOUND, "console bundle not embedded").into_response(),
    }
}

fn cache_control(path: &str) -> &'static str {
    if path == "index.html" || path.ends_with(".html") {
        "no-cache"
    } else if path.starts_with("assets/") {
        // Vite fingerprints asset file names; they never change in place.
        "public, max-age=31536000, immutable"
    } else {
        "public, max-age=3600, must-revalidate"
    }
}

fn serve(path: &str, data: std::borrow::Cow<'static, [u8]>, cache: &'static str) -> Response {
    let mime = mime_guess::from_path(path)
        .first_or_octet_stream()
        .essence_str()
        .to_string();
    let etag = format!("\"{}\"", crate::crypto::sha256_hex(&data)[..32].to_string());
    let mut res = Response::new(Body::from(data.into_owned()));
    let headers = res.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&mime).unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    if let Ok(v) = HeaderValue::from_str(&etag) {
        headers.insert(header::ETAG, v);
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn root_serves_index_html() {
        let r = static_handler("/".parse().unwrap()).await;
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(
            r.headers().get(header::CONTENT_TYPE).unwrap(),
            "text/html"
        );
    }

    #[tokio::test]
    async fn unknown_path_falls_back_to_shell() {
        let r = static_handler("/some/deep/route".parse().unwrap()).await;
        assert_eq!(r.status(), StatusCode::OK);
    }

    #[test]
    fn cache_policy_is_path_sensitive() {
        assert_eq!(cache_control("index.html"), "no-cache");
        assert!(cache_control("assets/index-abc.js").contains("immutable"));
        assert!(cache_control("favicon.ico").contains("must-revalidate"));
    }
}
