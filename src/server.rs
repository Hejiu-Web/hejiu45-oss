//! Router assembly, TLS termination, and graceful shutdown.
//!
//! ## Route ordering (the trap called out in the design notes)
//!
//! The SPA fallback is registered **last** via `.fallback(...)`, while
//! `/api/*` and the S3 routes are explicit. Axum 0.8 resolves explicit routes
//! before the fallback, so `/api/...` can never be swallowed by the SPA.
//! S3 bucket routes use `/{bucket}` and `/{bucket}/{*key}` — single-segment
//! and two-plus-segment — which keeps `/api` out of the S3 namespace entirely.

use anyhow::Context;
use axum::Router;
use axum::middleware;
use axum::routing::{any, get};
use tower_http::trace::TraceLayer;
use tracing::info;

use crate::config::TlsMode;
use crate::state::{AppState, capture_raw_path, security_headers};

/// Build the full application router.
///
/// Route table, in match-priority order:
/// * `/healthz` — liveness (no auth)
/// * `/api/*` — console API (cookie session)
/// * `<s3_prefix>/*` — S3 data plane (SigV4)
/// * everything else — the embedded SPA
///
/// The S3 prefix is applied with `nest`, which **strips it before routing**:
/// handlers see `/{bucket}/{key}` as the URI while the raw request target
/// (with the prefix) is preserved for SigV4 by [`capture_raw_path`].
pub fn build_router(state: AppState) -> Router {
    let mut router = Router::new()
        .route("/healthz", get(crate::s3::health))
        .merge(crate::admin::router());

    if state.args.s3_prefix.is_empty() {
        // S3 at the root: only appropriate for a dedicated host or port where
        // the console is not served. The console then keeps `/api` + assets.
        router = router.merge(s3_routes());
    } else {
        router = router.nest(&state.args.s3_prefix, s3_routes());
    }

    router
        // Anything under the S3 prefix that did not match an operation
        // (notably the bare `/s3/` spelling) must produce an S3 error rather
        // than silently returning the console shell, otherwise a misconfigured
        // client gets HTML with a 200 and cannot diagnose the problem.
        .fallback(s3_or_spa)
        .layer(middleware::from_fn(capture_raw_path))
        .layer(middleware::from_fn(security_headers))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// Final fallback: S3-prefixed paths get an S3 response, everything else the
/// SPA shell — unless the request targets a publicly readable bucket.
///
/// ## Public short links
///
/// A `public-read` bucket is meant to be shared, and
/// `https://host/s3/bucket/pic.png` is a worse link than
/// `https://host/bucket/pic.png`. So an unmatched path whose first segment
/// names a **public** bucket is served as an object read from that bucket.
///
/// This opens no new surface: exactly the same handler runs, with the same
/// `public-read` check inside it. A private bucket's name in that position
/// falls through to the SPA, which is indistinguishable from an unknown path —
/// bucket names are not leaked to anonymous callers this way.
///
/// The S3 *API* (`/s3/...`) keeps its full error semantics, so a client that
/// mistypes a URL still gets XML rather than HTML.
async fn s3_or_spa(
    state: axum::extract::State<AppState>,
    req: axum::extract::Request,
) -> axum::response::Response {
    let prefix = &state.args.s3_prefix;
    if !prefix.is_empty()
        && (req.uri().path() == prefix || req.uri().path().starts_with(&format!("{prefix}/")))
    {
        return crate::s3::root(state, req).await;
    }

    // Custom domain: the whole path is the object key of the bound bucket.
    if let Some(bucket) = custom_domain_bucket(&state, &req) {
        // Own the key so the borrow of `req` ends before it is moved.
        let key = req.uri().path().trim_start_matches('/').to_string();
        if !key.is_empty() {
            return crate::s3::public_object(state, req, &bucket, &key).await;
        }
    }

    // Short link: `/{bucket}/{key}` where the bucket is public.
    if let Some((bucket, key)) = public_short_link(&state, req.uri().path()) {
        return crate::s3::public_object(state, req, &bucket, &key).await;
    }

    crate::web::static_handler(req.uri().clone()).await
}

/// Split `/{bucket}/{key}` and return it only when `bucket` exists **and** is
/// publicly readable.
fn public_short_link(state: &AppState, path: &str) -> Option<(String, String)> {
    let rest = path.trim_start_matches('/');
    let (bucket, key) = rest.split_once('/')?;
    // Never shadow the console's own namespaces.
    if bucket.is_empty() || bucket == "api" || bucket == "assets" {
        return None;
    }
    if key.is_empty() {
        return None;
    }
    let meta: crate::meta::BucketMeta = state
        .meta
        .get_json(crate::meta::BUCKETS, bucket)
        .ok()
        .flatten()?;
    if meta.is_public_read() {
        Some((bucket.to_string(), key.to_string()))
    } else {
        None
    }
}

/// Resolve the bucket bound to this request's `Host`, if any.
///
/// The port is stripped so `cdn.example.com:443` matches `cdn.example.com`.
fn custom_domain_bucket(
    state: &AppState,
    req: &axum::http::Request<axum::body::Body>,
) -> Option<String> {
    let host = req
        .headers()
        .get(axum::http::header::HOST)
        .and_then(|v| v.to_str().ok())?
        .split(':')
        .next()?
        .to_ascii_lowercase();
    if host.is_empty() {
        return None;
    }
    let buckets: Vec<(String, crate::meta::BucketMeta)> = state
        .meta
        .scan_json(crate::meta::BUCKETS, None, 10_000)
        .ok()?;
    buckets
        .into_iter()
        .find(|(_, b)| !b.custom_domain.is_empty() && b.custom_domain.eq_ignore_ascii_case(&host))
        .map(|(_, b)| b.name)
}

/// The S3 route table, mounted at [`crate::config::Args::s3_prefix`].
///
/// `nest` strips the prefix before matching, so `/s3` and `/s3/` both arrive
/// here as `/` — a single route covers ListBuckets under either spelling.
fn s3_routes() -> Router<AppState> {
    Router::new()
        .route("/", any(crate::s3::root))
        .route("/{bucket}", any(crate::s3::bucket))
        .route("/{bucket}/{*key}", any(crate::s3::object))
}

/// Run HTTP (and optionally HTTPS) until SIGINT/SIGTERM.
pub async fn serve(state: AppState) -> anyhow::Result<()> {
    let router = build_router(state.clone());
    let addr = state.args.listen;

    match state.args.tls {
        TlsMode::Manual => {
            let cert = state.args.tls_cert.clone().expect("validated by clap");
            let key = state.args.tls_key.clone().expect("validated by clap");
            info!(%addr, "serving HTTPS");
            serve_tls(router, addr, &cert, &key).await
        }
        _ => {
            info!(%addr, "serving HTTP");
            let listener = tokio::net::TcpListener::bind(addr).await?;
            axum::serve(
                listener,
                router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .with_graceful_shutdown(shutdown_signal())
            .await?;
            Ok(())
        }
    }
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c().await.ok();
    };
    #[cfg(unix)]
    let term = async {
        use tokio::signal::unix::{SignalKind, signal};
        if let Ok(mut s) = signal(SignalKind::terminate()) {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {},
        _ = term => {},

    }
    info!("shutdown signal received; draining connections");
}

/// Serve HTTPS with a PEM certificate chain and private key.
///
/// This is the "no reverse proxy" deployment: point `--tls-cert`/`--tls-key`
/// at a certificate from Let's Encrypt (certbot), an internal CA, or a
/// corporate PKI. `axum-server` reloads the certificate from disk periodically,
/// so a renewal takes effect without a restart.
async fn serve_tls(
    router: Router,
    addr: std::net::SocketAddr,
    cert: &std::path::Path,
    key: &std::path::Path,
) -> anyhow::Result<()> {
    use axum_server::tls_rustls::RustlsConfig;

    let config = RustlsConfig::from_pem_file(cert, key)
        .await
        .with_context(|| format!("loading TLS certificate from {}", cert.display()))?;
    info!(cert = %cert.display(), "TLS configured (reloaded from disk every 60s)");

    let handle = axum_server::Handle::new();
    let shutdown_handle = handle.clone();
    tokio::spawn(async move {
        shutdown_signal().await;
        shutdown_handle.graceful_shutdown(Some(std::time::Duration::from_secs(10)));
    });

    axum_server::bind_rustls(addr, config)
        .handle(handle)
        .serve(router.into_make_service_with_connect_info::<std::net::SocketAddr>())
        .await?;
    Ok(())
}
