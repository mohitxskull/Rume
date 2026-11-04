//! Live TCP TestServer integration suite.
//!
//! Validates:
//! - Ephemeral TCP socket binding and external client requests
//! - Full Rume middleware execution (ObservabilityLayer, W3C TraceContext, RequestId)
//! - JSON request and response round-tripping
//! - Persistent connections (keep-alive)
//! - Chunked responses and streaming bodies
//! - Large request and response bodies
//! - Client disconnect during response streaming
//! - Graceful TCP server shutdown with active in-flight requests

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::Json;
use futures_util::stream;
use rume::app::Rume;
use rume::testing::TestServer;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

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

#[tokio::test]
async fn test_persistent_connections_keep_alive() {
    let app = Rume::new().route("/ping", get(|| async { "pong" }));
    let server = TestServer::from_builder(app).await.expect("start server");

    // Execute multiple sequential requests using the shared keep-alive client
    for _ in 0..10 {
        let resp = server.get("/ping").await.expect("ping succeeds");
        resp.assert_status(StatusCode::OK).assert_text_equals("pong");
    }

    server.shutdown().await.expect("shutdown succeeds");
}

#[tokio::test]
async fn test_chunked_response_streaming() {
    let app = Rume::new().route(
        "/stream",
        get(|| async {
            let chunks = vec![
                Ok::<_, std::io::Error>("chunk1,"),
                Ok("chunk2,"),
                Ok("chunk3"),
            ];
            let stream = stream::iter(chunks);
            Body::from_stream(stream).into_response()
        }),
    );

    let server = TestServer::from_builder(app).await.expect("start server");
    let resp = server.get("/stream").await.expect("stream succeeds");
    resp.assert_status(StatusCode::OK)
        .assert_text_equals("chunk1,chunk2,chunk3");

    server.shutdown().await.expect("shutdown succeeds");
}

#[tokio::test]
async fn test_large_request_and_response_bodies() {
    let app = Rume::new().route(
        "/echo-bytes",
        post(|body: axum::body::Bytes| async move { body }),
    );

    let server = TestServer::from_builder(app).await.expect("start server");

    // 2 megabytes of test bytes
    let payload = vec![0xABu8; 2 * 1024 * 1024];

    let req = Request::builder()
        .method(Method::POST)
        .uri("/echo-bytes")
        .header("content-type", "application/octet-stream")
        .body(Body::from(payload.clone()))
        .expect("build request");

    let resp = server.request(req).await.expect("post large body succeeds");
    let resp = resp.assert_status(StatusCode::OK);
    assert_eq!(resp.bytes(), payload.as_slice());

    server.shutdown().await.expect("shutdown succeeds");
}

#[tokio::test]
async fn test_client_disconnect_during_response_streaming() {
    let client_disconnected = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&client_disconnected);

    let app = Rume::new()
        .route(
            "/slow-stream",
            get(move || {
                let flag = Arc::clone(&flag);
                async move {
                    let stream = stream::unfold((0, flag), |(step, flag)| async move {
                        if step >= 20 {
                            return None;
                        }
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        flag.store(true, Ordering::SeqCst);
                        Some((Ok::<_, std::io::Error>(format!("data-{step}\n")), (step + 1, flag)))
                    });
                    Body::from_stream(stream).into_response()
                }
            }),
        )
        .route("/health", get(|| async { "healthy" }));

    let server = TestServer::from_builder(app).await.expect("start server");

    // Open a raw TCP socket and read only the first chunk, then drop the socket
    {
        let mut raw = server.connect_raw().await.expect("connect raw socket");
        raw.write_all(b"GET /slow-stream HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
            .await
            .expect("write request");

        let mut buf = [0u8; 256];
        let n = raw.read(&mut buf).await.expect("read initial chunk");
        assert!(n > 0);
        // Abruptly drop socket to simulate client disconnect
        drop(raw);
    }

    // Give server a moment to observe disconnect and clean up task
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Server remains completely healthy and handles subsequent traffic
    let health = server.get("/health").await.expect("health succeeds");
    health.assert_status(StatusCode::OK).assert_text_equals("healthy");

    server.shutdown().await.expect("shutdown succeeds");
}

#[tokio::test]
async fn test_graceful_shutdown_with_active_in_flight_requests() {
    let app = Rume::new().route(
        "/slow-work",
        get(|| async {
            tokio::time::sleep(Duration::from_millis(150)).await;
            "work-completed"
        }),
    );

    let server = TestServer::from_builder(app).await.expect("start server");

    let client = server.client().clone();
    let url = server.url("/slow-work");

    // Start in-flight request
    let request_task = tokio::spawn(async move {
        client.get(&url).send().await
    });

    // Wait 30ms to ensure the request has arrived and is executing
    tokio::time::sleep(Duration::from_millis(30)).await;

    // Trigger graceful shutdown while the request is still executing
    server.shutdown().await.expect("shutdown initiated");

    // Verify the in-flight request completed successfully before termination
    let resp = request_task.await.expect("task joins").expect("request finishes");
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "work-completed");
}
