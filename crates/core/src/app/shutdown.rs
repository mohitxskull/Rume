//! Shutdown signal handling and operational tracing re-exports.

pub use crate::app::observability::{init_tracing, try_init_tracing};

// Why: `ops_router` was removed because `crate::routing::health_routes` is the
// canonical liveness/readiness surface and `ops_router` had zero workspace callers.

/// Wait for a shutdown signal (Ctrl+C or SIGTERM on Unix).
pub(crate) async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut signal) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            signal.recv().await;
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
}
