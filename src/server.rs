//! Router assembly, TLS termination, and graceful shutdown.
//!
//! ## Route ordering (the trap called out in the design notes)
//!
//! The SPA fallback is registered **last** via `.fallback(...)`, while
//! `/api/*` and the S3 routes are explicit. Axum 0.8 resolves explicit routes
//! before the fallback, so `/api/...` can never be swallowed by the SPA.
//! S3 bucket routes use `/{bucket}` and `/{bucket}/{*key}` — single-segment
//! and two-plus-segment — which keeps `/api` out of the S3 namespace entirely.

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
/// SPA shell.
async fn s3_or_spa(state: axum::extract::State<AppState>, req: axum::extract::Request) -> axum::response::Response {
    let prefix = &state.args.s3_prefix;
    if !prefix.is_empty() && (req.uri().path() == prefix || req.uri().path().starts_with(&format!("{prefix}/"))) {
        return crate::s3::root(state, req).await;
    }
    crate::web::static_handler(req.uri().clone()).await
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
            axum::serve(listener, router.into_make_service_with_connect_info::<std::net::SocketAddr>())
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

#[allow(unused_variables)]
async fn serve_tls(router: Router, addr: std::net::SocketAddr, cert: &std::path::Path, key: &std::path::Path) -> anyhow::Result<()> {
    anyhow::bail!("TLS support is built in a later step of this build")
}





