//! Prometheus and OpenMetrics `/metrics` HTTP scrape endpoint helpers.

use axum::extract::{FromRef, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use rume_observability::{
    MetricsRegistry, OPENMETRICS_CONTENT_TYPE, PROMETHEUS_CONTENT_TYPE, PrometheusFormat,
    render_registry,
};

use crate::routing::Router;

const OPENMETRICS_MEDIA_TYPE: &str = "application/openmetrics-text";

/// Mounts the default `GET /metrics` scrape route extracting [`MetricsRegistry`] via [`FromRef`].
///
/// The route is open by default so it composes cleanly with user middleware
/// (for example bearer auth, IP allowlists, or a dedicated admin listener).
pub fn metrics_routes<S>() -> Router<S>
where
    S: Clone + Send + Sync + 'static,
    MetricsRegistry: FromRef<S>,
{
    metrics_routes_at("/metrics")
}

/// Mounts the metrics scrape route at a custom path extracting [`MetricsRegistry`] via [`FromRef`].
pub fn metrics_routes_at<S>(path: &str) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
    MetricsRegistry: FromRef<S>,
{
    Router::new().get(path, metrics_handler)
}

/// Axum handler rendering [`MetricsRegistry`] in Prometheus 0.0.4 or OpenMetrics 1.0 text format.
///
/// Negotiates OpenMetrics 1.0 when any `Accept` header contains
/// `application/openmetrics-text`; otherwise responds with Prometheus 0.0.4 text format.
pub async fn metrics_handler(
    State(registry): State<MetricsRegistry>,
    headers: HeaderMap,
) -> Response {
    let wants_openmetrics = headers
        .get_all(header::ACCEPT)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .any(|v| v.to_ascii_lowercase().contains(OPENMETRICS_MEDIA_TYPE));

    let (format, content_type) = if wants_openmetrics {
        (PrometheusFormat::OpenMetrics100, OPENMETRICS_CONTENT_TYPE)
    } else {
        (PrometheusFormat::Text004, PROMETHEUS_CONTENT_TYPE)
    };

    let body = render_registry(&registry, format);

    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, "no-store"),
        ],
        body,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use axum::body::Body;
    use axum::http::{Request, StatusCode, header};
    use rume_foundation::telemetry::PoolStatus;
    use rume_observability::{
        DynamicPoolExporter, MetricsRegistry, OPENMETRICS_CONTENT_TYPE, PROMETHEUS_CONTENT_TYPE,
    };
    use tower::ServiceExt;

    use super::*;

    #[tokio::test]
    async fn metrics_routes_serves_prometheus_text_by_default() {
        let registry = MetricsRegistry::new();
        registry.record_http_request("GET", 200, Duration::from_millis(15));

        let router = metrics_routes::<MetricsRegistry>().with_state(registry);
        let request = Request::builder()
            .uri("/metrics")
            .body(Body::empty())
            .unwrap();
        let response = router.oneshot(request).await.expect("request failed");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            PROMETHEUS_CONTENT_TYPE
        );
        assert_eq!(
            response.headers().get(header::CACHE_CONTROL).unwrap(),
            "no-store"
        );

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let text = String::from_utf8(body.to_vec()).unwrap();
        assert!(
            text.contains("http_requests_total"),
            "expected http_requests_total in body:\n{text}"
        );
    }

    #[tokio::test]
    async fn metrics_routes_negotiates_openmetrics_when_requested() {
        let registry = MetricsRegistry::new();
        registry.record_http_request("GET", 200, Duration::from_millis(15));

        let router = metrics_routes::<MetricsRegistry>().with_state(registry);
        let request = Request::builder()
            .uri("/metrics")
            .header(
                header::ACCEPT,
                "application/openmetrics-text; version=1.0.0",
            )
            .body(Body::empty())
            .unwrap();
        let response = router.oneshot(request).await.expect("request failed");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            OPENMETRICS_CONTENT_TYPE
        );
        assert_eq!(
            response.headers().get(header::CACHE_CONTROL).unwrap(),
            "no-store"
        );

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let text = String::from_utf8(body.to_vec()).unwrap();
        assert!(
            text.ends_with("# EOF\n"),
            "expected OpenMetrics body to end with # EOF\\n:\n{text}"
        );
    }

    #[tokio::test]
    async fn metrics_routes_refreshes_registered_pool_exporters_on_scrape() {
        let registry = MetricsRegistry::new();
        let exporter = Arc::new(DynamicPoolExporter::new("primary", || PoolStatus {
            leased_connections: 1,
            active_connections: 2,
            idle_connections: 8,
            total_connections: 10,
            max_connections: 20,
            in_flight_transactions: 0,
            is_closed: false,
        }));
        registry.register_pool_exporter(exporter);

        let router = metrics_routes::<MetricsRegistry>().with_state(registry);
        let request = Request::builder()
            .uri("/metrics")
            .body(Body::empty())
            .unwrap();
        let response = router.oneshot(request).await.expect("request failed");

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let text = String::from_utf8(body.to_vec()).unwrap();
        assert!(
            text.contains("db_pool_connections"),
            "expected db_pool_connections in scraped payload:\n{text}"
        );
    }
}
