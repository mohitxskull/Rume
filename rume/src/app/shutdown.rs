//! Shutdown signal handling, operational routes, and tracing initialization.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json};
use axum::routing::{Router, get};
use serde_json::json;
use tracing_subscriber::{EnvFilter, fmt};

use crate::app::state::{Availability, AvailabilityState};

/// Initialize the tracing subscriber.
///
/// Reads `RUST_LOG` (or defaults to `info`) to set the filtering level, then
/// installs a `fmt` subscriber with that filter.
pub fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    fmt::fmt().with_env_filter(filter).init();
}

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

/// Wrap a router with ops routes (liveness/readiness) keyed on availability.
///
/// The router's state type must be [`Availability`], since the route handlers
/// extract it via [`State`](axum::extract::State).
///
/// # Example
///
/// ```
/// use rume::app::ops_router;
/// use axum::Router;
///
/// let router = ops_router(Router::new());
/// ```
pub fn ops_router(router: Router<Availability>) -> Router<Availability> {
    router
        .route(
            "/ops/live",
            get(move |State(av): State<Availability>| async move {
                if av.state() == AvailabilityState::Broken {
                    (
                        StatusCode::SERVICE_UNAVAILABLE,
                        Json(json!({"status": "broken"})),
                    )
                        .into_response()
                } else {
                    (StatusCode::OK, Json(json!({"status": "alive"}))).into_response()
                }
            }),
        )
        .route(
            "/ops/ready",
            get(move |State(av): State<Availability>| async move {
                if av.state() == AvailabilityState::Ready {
                    (StatusCode::OK, Json(json!({"status": "ready"}))).into_response()
                } else {
                    (
                        StatusCode::SERVICE_UNAVAILABLE,
                        Json(json!({"status": "not_ready"})),
                    )
                        .into_response()
                }
            }),
        )
}
