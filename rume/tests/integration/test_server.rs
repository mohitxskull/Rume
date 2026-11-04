//! Live TCP TestServer integration suite.
//!
//! Validates:
//! - Ephemeral TCP socket binding and external client requests
//! - Full Rume middleware execution (ObservabilityLayer, W3C TraceContext, RequestId)
//! - JSON request and response round-tripping
//! - Graceful TCP server shutdown

use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::Json;
use rume::app::Rume;
use rume::testing::TestServer;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct GreetingRequest {
    name: String,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct GreetingResponse {
    greeting: String,
}

#[tokio::test]
async fn live_test_server_exercises_middleware_and_sockets() {
    let app = Rume::new()
        .route("/health", get(|| async { "ok" }))
        .route(
            "/greet",
            post(|Json(req): Json<GreetingRequest>| async move {
                Json(GreetingResponse {
                    greeting: format!("Hello, {}!", req.name),
                })
            }),
        );

    let server = TestServer::from_builder(app)
        .await
        .expect("start live test server from builder");

    assert!(server.local_addr().port() > 0);

    // 1. Verify GET over real TCP with middleware
    let health_resp = server
        .get("/health")
        .await
        .expect("get request succeeds")
        .assert_status(StatusCode::OK)
        .assert_text_equals("ok");

    // Observability middleware injects x-request-id
    assert!(health_resp.headers().contains_key("x-request-id"));
    assert!(health_resp.headers().contains_key("traceparent"));

    // 2. Verify POST JSON over real TCP
    let greet_resp = server
        .post_json("/greet", r#"{"name":"Rume Developer"}"#)
        .await
        .expect("post json succeeds")
        .assert_status(StatusCode::OK)
        .assert_header("content-type", "application/json");

    let greeting: GreetingResponse = greet_resp.json();
    assert_eq!(
        greeting,
        GreetingResponse {
            greeting: "Hello, Rume Developer!".to_string(),
        }
    );

    // 3. Gracefully shutdown the server
    server.shutdown().await.expect("graceful shutdown succeeds");
}
