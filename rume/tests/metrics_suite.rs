use axum::http::StatusCode;
use axum::routing::post;
use rume::metrics::{Counter, Gauge, MetricsLayer, MetricsRegistry};
use rume::prelude::*;

#[tokio::test]
async fn counter_and_gauge_primitives() {
    let counter = Counter::default();
    assert_eq!(counter.get(), 0);
    counter.inc();
    counter.inc_by(5);
    assert_eq!(counter.get(), 6);

    let gauge = Gauge::default();
    assert_eq!(gauge.get(), 0);
    gauge.inc();
    gauge.inc();
    gauge.dec();
    assert_eq!(gauge.get(), 1);
    gauge.set(42);
    assert_eq!(gauge.get(), 42);
}

#[tokio::test]
async fn metrics_middleware_and_prometheus_exporter() {
    let registry = MetricsRegistry::new();

    let router = rume::routing::Router::new()
        .get("/users", || async { "list users" })
        .route(
            "/users",
            post(|| async { (StatusCode::CREATED, "user created") }),
        )
        .metrics("/metrics", registry.clone())
        .layer(MetricsLayer::new(registry.clone()));

    let app = TestApp::new(router);

    // Initial metrics scrape: in_flight = 0
    let res0 = app.get("/metrics").await;
    assert_eq!(res0.status(), StatusCode::OK);
    let body0 = TestApp::body_text(res0).await;
    assert!(body0.contains("http_requests_in_flight 0"));
    assert_eq!(registry.in_flight(), 0);

    // Send 2 GET /users requests
    let _ = app.get("/users").await;
    let _ = app.get("/users").await;

    // Send 1 POST /users request
    let req = axum::http::Request::builder()
        .method("POST")
        .uri("/users")
        .body(axum::body::Body::empty())
        .unwrap();
    let _ = app.request(req).await;

    // Scrape /metrics again
    let res = app.get("/metrics").await;
    assert_eq!(res.status(), StatusCode::OK);
    let body = TestApp::body_text(res).await;

    // Check Prometheus text output
    assert!(body.contains("http_requests_total{method=\"GET\",status=\"200\"} 2"));
    assert!(body.contains("http_requests_total{method=\"POST\",status=\"201\"} 1"));
    assert!(body.contains("http_request_duration_seconds_count{method=\"GET\"} 2"));
    assert!(body.contains("http_request_duration_seconds_count{method=\"POST\"} 1"));
}
