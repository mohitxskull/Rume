//! Shutdown signal handling and operational tracing re-exports.

pub use crate::app::observability::{init_tracing, try_init_tracing};

// Why: `ops_router` was removed because `crate::routing::health_routes` is the
// canonical liveness/readiness surface and `ops_router` had zero workspace callers.

/// Wait for a shutdown signal (Ctrl+C or SIGTERM on Unix).
pub(crate) async fn shutdown_signal() {
    // Why: if registering a signal handler fails (e.g. restricted container or
    // unsupported signal environment), log and pend forever instead of resolving
    // immediately — resolving `()` would terminate the server during boot.
    let ctrl_c = async {
        if let Err(err) = tokio::signal::ctrl_c().await {
            tracing::error!(error = %err, "Failed to install Ctrl+C signal handler");
            std::future::pending::<()>().await;
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(err) => {
                tracing::error!(error = %err, "Failed to install SIGTERM signal handler");
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
}
