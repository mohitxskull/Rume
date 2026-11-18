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
async fn metrics_middleware_and_otel_registry() {
    let registry = MetricsRegistry::new();

    let router = rume::routing::Router::new()
        .get("/users", || async { "list users" })
        .route(
            "/users",
            post(|| async { (StatusCode::CREATED, "user created") }),
        )
        .layer(MetricsLayer::new(registry.clone()));

    let app = TestApp::new(router);

    // Initial snapshot: in_flight = 0
    let snap0 = registry.snapshot();
    assert_eq!(snap0.gauge_i64("http_requests_in_flight", &[]), Some(0));
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

    // Export OTel snapshot and verify counters & histograms
    let snap = registry.snapshot();
    assert_eq!(
        snap.counter_u64(
            "http_requests_total",
            &[("method", "GET"), ("status", "200")]
        ),
        Some(2)
    );
    assert_eq!(
        snap.counter_u64(
            "http_requests_total",
            &[("method", "POST"), ("status", "201")]
        ),
        Some(1)
    );
    let (get_count, _, _, _) = snap
        .histogram_f64("http_request_duration_seconds", &[("method", "GET")])
        .expect("GET duration histogram recorded");
    assert_eq!(get_count, 2);
    let (post_count, _, _, _) = snap
        .histogram_f64("http_request_duration_seconds", &[("method", "POST")])
        .expect("POST duration histogram recorded");
    assert_eq!(post_count, 1);
}
