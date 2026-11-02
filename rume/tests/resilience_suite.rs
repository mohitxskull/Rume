use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::http::StatusCode;
use rume::prelude::*;
use tokio::time::sleep;

#[tokio::test]
async fn circuit_breaker_state_transitions_and_trip() {
    let config = CircuitBreakerConfig {
        failure_threshold: 3,
        success_threshold: 2,
        reset_timeout: Duration::from_millis(50),
    };
    let breaker = CircuitBreaker::new(config);

    let fail_flag = Arc::new(AtomicUsize::new(0));

    let flag_clone = Arc::clone(&fail_flag);
    let router = rume::routing::Router::new()
        .get("/service", move || {
            let flag = Arc::clone(&flag_clone);
            async move {
                if flag.load(Ordering::SeqCst) == 1 {
                    (StatusCode::INTERNAL_SERVER_ERROR, "downstream error")
                } else {
                    (StatusCode::OK, "service healthy")
                }
            }
        })
        .layer(CircuitBreakerLayer::new(breaker.clone()));

    let app = TestApp::new(router);

    // Initial state: Closed
    assert_eq!(breaker.state(), CircuitState::Closed);
    let res = app.get("/service").await;
    assert_eq!(res.status(), StatusCode::OK);

    // Trigger 3 consecutive 500 errors
    fail_flag.store(1, Ordering::SeqCst);
    for _ in 0..3 {
        let res = app.get("/service").await;
        assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    // Breaker should now be OPEN
    assert_eq!(breaker.state(), CircuitState::Open);

    // Request is rejected immediately with 503 SERVICE_UNAVAILABLE
    let res = app.get("/service").await;
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    let val: serde_json::Value = serde_json::from_str(&TestApp::body_text(res).await).unwrap();
    assert_eq!(val["code"], "CIRCUIT_BREAKER_OPEN");

    // Wait past reset timeout (50ms) -> transitions to HalfOpen
    sleep(Duration::from_millis(60)).await;
    assert_eq!(breaker.state(), CircuitState::HalfOpen);

    // Restore downstream service
    fail_flag.store(0, Ordering::SeqCst);

    // 2 successes in HalfOpen close the circuit
    let res1 = app.get("/service").await;
    assert_eq!(res1.status(), StatusCode::OK);
    let res2 = app.get("/service").await;
    assert_eq!(res2.status(), StatusCode::OK);

    assert_eq!(breaker.state(), CircuitState::Closed);
}

#[tokio::test]
async fn rate_limiting_sliding_window_and_headers() {
    let limiter = RateLimiter::new(RateLimitConfig {
        max_requests: 3,
        window: Duration::from_secs(60),
    });

    let router = rume::routing::Router::new()
        .get("/api/data", || async { "payload" })
        .layer(RateLimitLayer::new(limiter));

    let app = TestApp::new(router);

    // Request 1: 200 OK, remaining = 2
    let req1 = axum::http::Request::builder()
        .uri("/api/data")
        .header("x-forwarded-for", "192.168.1.100")
        .body(axum::body::Body::empty())
        .unwrap();
    let res1 = app.request(req1).await;
    assert_eq!(res1.status(), StatusCode::OK);
    assert_eq!(res1.headers().get("x-ratelimit-remaining").unwrap(), "2");

    // Request 2: 200 OK, remaining = 1
    let req2 = axum::http::Request::builder()
        .uri("/api/data")
        .header("x-forwarded-for", "192.168.1.100")
        .body(axum::body::Body::empty())
        .unwrap();
    let res2 = app.request(req2).await;
    assert_eq!(res2.status(), StatusCode::OK);
    assert_eq!(res2.headers().get("x-ratelimit-remaining").unwrap(), "1");

    // Request 3: 200 OK, remaining = 0
    let req3 = axum::http::Request::builder()
        .uri("/api/data")
        .header("x-forwarded-for", "192.168.1.100")
        .body(axum::body::Body::empty())
        .unwrap();
    let res3 = app.request(req3).await;
    assert_eq!(res3.status(), StatusCode::OK);
    assert_eq!(res3.headers().get("x-ratelimit-remaining").unwrap(), "0");

    // Request 4: 429 TOO_MANY_REQUESTS
    let req4 = axum::http::Request::builder()
        .uri("/api/data")
        .header("x-forwarded-for", "192.168.1.100")
        .body(axum::body::Body::empty())
        .unwrap();
    let res4 = app.request(req4).await;
    assert_eq!(res4.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(res4.headers().contains_key("retry-after"));
    let val: serde_json::Value = serde_json::from_str(&TestApp::body_text(res4).await).unwrap();
    assert_eq!(val["code"], "RATE_LIMIT_EXCEEDED");
}

#[tokio::test]
async fn concurrency_limiting_and_load_shedding() {
    let _limiter = ConcurrencyLimiter::new(2);

    let router = rume::routing::Router::new()
        .get("/compute", || async {
            sleep(Duration::from_millis(60)).await;
            "computed"
        })
        .layer(ConcurrencyLimitLayer::new(2));

    let app = Arc::new(TestApp::new(router));

    // Spawn 2 long requests to saturate capacity
    let app1 = Arc::clone(&app);
    let h1 = tokio::spawn(async move { app1.get("/compute").await });

    let app2 = Arc::clone(&app);
    let h2 = tokio::spawn(async move { app2.get("/compute").await });

    // Brief sleep to let both acquire permits
    sleep(Duration::from_millis(15)).await;

    // 3rd concurrent request exceeds limit of 2 -> 503 SHED_LOAD
    let res3 = app.get("/compute").await;
    assert_eq!(res3.status(), StatusCode::SERVICE_UNAVAILABLE);
    let val: serde_json::Value = serde_json::from_str(&TestApp::body_text(res3).await).unwrap();
    assert_eq!(val["code"], "SHED_LOAD");

    // Wait for in-flight requests to complete
    let (r1, r2) = tokio::join!(h1, h2);
    assert_eq!(r1.unwrap().status(), StatusCode::OK);
    assert_eq!(r2.unwrap().status(), StatusCode::OK);

    // After permits freed, next request succeeds
    let res4 = app.get("/compute").await;
    assert_eq!(res4.status(), StatusCode::OK);
}

#[tokio::test]
async fn request_timeout_middleware() {
    let router = rume::routing::Router::new()
        .get("/fast", || async { "fast" })
        .get("/slow", || async {
            sleep(Duration::from_millis(80)).await;
            "slow"
        })
        .layer(TimeoutLayer::new(Duration::from_millis(30)));

    let app = TestApp::new(router);

    // Fast request completes within 30ms -> 200 OK
    let res_fast = app.get("/fast").await;
    assert_eq!(res_fast.status(), StatusCode::OK);

    // Slow request exceeds 30ms -> 504 GATEWAY_TIMEOUT Problem Details
    let res_slow = app.get("/slow").await;
    assert_eq!(res_slow.status(), StatusCode::GATEWAY_TIMEOUT);
    let val: serde_json::Value = serde_json::from_str(&TestApp::body_text(res_slow).await).unwrap();
    assert_eq!(val["code"], "TIMEOUT");
}
