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
pub fn build_router(state: AppState) -> Router {
    let s3_routes = Router::new()
        .route("/healthz", get(crate::s3::health))
        .route("/", any(crate::s3::root))
        .route("/{bucket}", any(crate::s3::bucket))
        .route("/{bucket}/{*key}", any(crate::s3::object));

    Router::new()
        .merge(crate::admin::router())
        .merge(s3_routes)
        .fallback(crate::web::static_handler)
        .layer(middleware::from_fn(capture_raw_path))
        .layer(middleware::from_fn(security_headers))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
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


