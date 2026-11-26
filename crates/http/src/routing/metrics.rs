//! Prometheus and OpenMetrics `/metrics` HTTP scrape endpoint helpers.

use axum::extract::{FromRef, State};
use axum::http::HeaderMap;
use axum::response::Response;
use rume_observability::MetricsRegistry;

use crate::routing::Router;

/// Mounts the default `GET /metrics` scrape route extracting [`MetricsRegistry`] via [`FromRef`].
#[allow(unused_variables, dead_code)] // Removed in Task 03.
pub fn metrics_routes<S>() -> Router<S>
where
    S: Clone + Send + Sync + 'static,
    MetricsRegistry: FromRef<S>,
{
    todo!("implemented in Task 03")
}

/// Mounts the metrics scrape route at a custom path extracting [`MetricsRegistry`] via [`FromRef`].
#[allow(unused_variables, dead_code)] // Removed in Task 03.
pub fn metrics_routes_at<S>(path: &str) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
    MetricsRegistry: FromRef<S>,
{
    todo!("implemented in Task 03")
}

/// Axum handler rendering [`MetricsRegistry`] in Prometheus 0.0.4 or OpenMetrics 1.0 text format.
#[allow(unused_variables, dead_code)] // Removed in Task 03.
pub async fn metrics_handler(
    State(registry): State<MetricsRegistry>,
    headers: HeaderMap,
) -> Response {
    todo!("implemented in Task 03")
}
