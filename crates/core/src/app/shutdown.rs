//! Shutdown signal handling and operational shutdown helpers.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json};
use axum::routing::{Router, get};
use serde_json::json;

use crate::app::state::{Availability, AvailabilityState};

pub use crate::app::observability::{init_tracing, try_init_tracing};

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
/// # Deprecated
///
/// Use canonical [`rume::routing::health_routes`](crate::routing::health_routes) instead.
#[deprecated(
    since = "0.2.0",
    note = "Use canonical `rume::routing::health_routes` instead"
)]
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
