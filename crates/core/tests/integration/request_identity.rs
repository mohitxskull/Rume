//! Integration tests for request identity, propagation, and observability.

use axum::http::header::HeaderName;
use axum::http::{HeaderValue, StatusCode};
use axum::routing::get;
use rume::app::{AppContext, RequestId, Rume, X_REQUEST_ID};
use rume::error::AppError;
use rume::testing::TestApp;
use serde_json::Value;

fn test_ctx() -> AppContext<(), (), ()> {
    AppContext::new(
        (),
        (),
        rume::app::ProjectPaths::from_current_dir(),
        rume::app::Availability::new(),
        rume::routing::HealthChecks::new(),
        (),
    )
}

async fn echo_request_id(request_id: RequestId) -> String {
    request_id.to_string()
}

async fn error_handler() -> Result<String, AppError> {
    Err(AppError::NotFound)
}

async fn bad_request_handler() -> Result<String, AppError> {
    Err(AppError::BadRequest {
        code: "INVALID_INPUT",
        message: "bad payload".to_string(),
    })
}

#[tokio::test]
async fn auto_generates_request_id_when_header_missing() {
    let ctx = test_ctx();
    let builder = Rume::builder(ctx).route("/echo", get(echo_request_id));
    let router = builder.build_router(test_ctx()).unwrap();
    let app = TestApp::new(router.with_state(test_ctx()));

    let response = app.get("/echo").await;
    assert_eq!(response.status(), StatusCode::OK);

    // Response must contain X-Request-ID header
    let header_val = response
        .headers()
        .get(&X_REQUEST_ID)
        .and_then(|v| v.to_str().ok())
        .expect("missing x-request-id header")
        .to_string();

    assert_eq!(header_val.len(), 36);

    let body = TestApp::body_text(response).await;
    // Extractor inside handler must receive the exact same generated RequestId
    assert_eq!(body, header_val);
}

#[tokio::test]
async fn preserves_safe_incoming_request_id() {
    let ctx = test_ctx();
    let builder = Rume::builder(ctx).route("/echo", get(echo_request_id));
    let router = builder.build_router(test_ctx()).unwrap();
    let app = TestApp::new(router.with_state(test_ctx()));

    let custom_id = "trace-client-12345-abcde";
    let req = axum::http::Request::builder()
        .uri("/echo")
        .header(
            HeaderName::from_static("x-request-id"),
            HeaderValue::from_static(custom_id),
        )
        .body(axum::body::Body::empty())
        .unwrap();

    let response = app.request(req).await;
    assert_eq!(response.status(), StatusCode::OK);

    let header_val = response
        .headers()
        .get(&X_REQUEST_ID)
        .and_then(|v| v.to_str().ok())
        .unwrap()
        .to_string();

    assert_eq!(header_val, custom_id);

    let body = TestApp::body_text(response).await;
    assert_eq!(body, custom_id);
}

#[tokio::test]
async fn replaces_unsafe_incoming_request_id_with_safe_uuid() {
    let ctx = test_ctx();
    let builder = Rume::builder(ctx).route("/echo", get(echo_request_id));
    let router = builder.build_router(test_ctx()).unwrap();
    let app = TestApp::new(router.with_state(test_ctx()));

    // Invalid request ID (contains spaces, angle brackets, and non-identifier characters)
    let unsafe_id = "<script>alert(1)</script> bad request id";
    let req = axum::http::Request::builder()
        .uri("/echo")
        .header("x-request-id", unsafe_id)
        .body(axum::body::Body::empty())
        .unwrap();

    let response = app.request(req).await;
    assert_eq!(response.status(), StatusCode::OK);

    let header_val = response
        .headers()
        .get(&X_REQUEST_ID)
        .and_then(|v| v.to_str().ok())
        .unwrap();

    // Must be replaced with a clean UUID, not the unsafe string
    assert_ne!(header_val, unsafe_id);
    assert_eq!(header_val.len(), 36);
}

#[tokio::test]
async fn attaches_request_id_to_problem_details_responses() {
    let ctx = test_ctx();
    let builder = Rume::builder(ctx)
        .route("/error", get(error_handler))
        .route("/bad-request", get(bad_request_handler));
    let router = builder.build_router(test_ctx()).unwrap();
    let app = TestApp::new(router.with_state(test_ctx()));

    let custom_id = "client-error-trace-999";
    let req = axum::http::Request::builder()
        .uri("/bad-request")
        .header("x-request-id", custom_id)
        .body(axum::body::Body::empty())
        .unwrap();

    let response = app.request(req).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let header_val = response
        .headers()
        .get(&X_REQUEST_ID)
        .and_then(|v| v.to_str().ok())
        .unwrap()
        .to_string();
    assert_eq!(header_val, custom_id);

    let body = TestApp::body_text(response).await;
    let json: Value = serde_json::from_str(&body).expect("invalid problem json");

    // Problem details must contain the exact matching request_id
    assert_eq!(
        json.get("request_id").and_then(|v| v.as_str()),
        Some(custom_id)
    );
    assert_eq!(
        json.get("code").and_then(|v| v.as_str()),
        Some("INVALID_INPUT")
    );
}

#[tokio::test]
async fn oversized_problem_details_preserves_body() {
    let ctx = test_ctx();
    // 2 MiB payload exceeds MAX_PROBLEM_DETAILS_INSPECT (1 MiB)
    let large_body = format!(
        r#"{{"title":"large error","detail":"{}"}}"#,
        "x".repeat(2 * 1024 * 1024)
    );
    let large_body_clone = large_body.clone();

    let handler = move || {
        let b = large_body_clone.clone();
        async move {
            axum::response::Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .header("content-type", "application/problem+json")
                .header("content-length", b.len().to_string())
                .body(axum::body::Body::from(b))
                .unwrap()
        }
    };

    let builder = Rume::builder(ctx).route("/large-problem", get(handler));
    let router = builder.build_router(test_ctx()).unwrap();
    let app = TestApp::new(router.with_state(test_ctx()));

    let response = app.get("/large-problem").await;
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);

    let body = TestApp::body_text(response).await;
    // The oversized body must NOT be truncated or replaced with empty body!
    assert_eq!(body.len(), large_body.len());
    assert_eq!(body, large_body);
}

#[tokio::test]
async fn explicit_content_length_is_accurately_updated_on_injection() {
    let ctx = test_ctx();
    let original_json = r#"{"type":"about:blank","title":"Bad Request"}"#;
    let original_len = original_json.len();

    let handler = move || async move {
        axum::response::Response::builder()
            .status(StatusCode::BAD_REQUEST)
            .header("content-type", "application/problem+json")
            .header("content-length", original_len.to_string())
            .body(axum::body::Body::from(original_json))
            .unwrap()
    };

    let builder = Rume::builder(ctx).route("/explicit-len", get(handler));
    let router = builder.build_router(test_ctx()).unwrap();
    let app = TestApp::new(router.with_state(test_ctx()));

    let response = app.get("/explicit-len").await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let content_len_hdr: usize = response
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .unwrap()
        .parse()
        .unwrap();

    let body = TestApp::body_text(response).await;
    // Header must match the actual body length after injection
    assert_eq!(content_len_hdr, body.len());
    assert!(body.contains("request_id"));
}

#[tokio::test]
async fn malformed_json_problem_details_preserves_body() {
    let ctx = test_ctx();
    let malformed = "this is not valid json {{{";

    let handler = move || async move {
        axum::response::Response::builder()
            .status(StatusCode::BAD_REQUEST)
            .header("content-type", "application/problem+json")
            .body(axum::body::Body::from(malformed))
            .unwrap()
    };

    let builder = Rume::builder(ctx).route("/malformed", get(handler));
    let router = builder.build_router(test_ctx()).unwrap();
    let app = TestApp::new(router.with_state(test_ctx()));

    let response = app.get("/malformed").await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let body = TestApp::body_text(response).await;
    // Malformed body must remain intact, not dropped or replaced
    assert_eq!(body, malformed);
}

#[tokio::test]
async fn ordinary_error_response_remains_untouched() {
    let ctx = test_ctx();
    let plain_error = "500 Internal Server Error: Database failure";

    let handler = move || async move {
        axum::response::Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .header("content-type", "text/plain; charset=utf-8")
            .body(axum::body::Body::from(plain_error))
            .unwrap()
    };

    let builder = Rume::builder(ctx).route("/plain-error", get(handler));
    let router = builder.build_router(test_ctx()).unwrap();
    let app = TestApp::new(router.with_state(test_ctx()));

    let response = app.get("/plain-error").await;
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);

    let body = TestApp::body_text(response).await;
    assert_eq!(body, plain_error);
}

#[tokio::test]
async fn concurrent_requests_have_independent_os_csprng_uuids() {
    let ctx = test_ctx();
    let builder = Rume::builder(ctx).route("/echo", get(echo_request_id));
    let router = builder.build_router(test_ctx()).unwrap();

    let mut handles = Vec::new();
    for _ in 0..50 {
        let router_clone = router.clone();
        handles.push(tokio::spawn(async move {
            let app = TestApp::new(router_clone.with_state(test_ctx()));
            let resp = app.get("/echo").await;
            assert_eq!(resp.status(), StatusCode::OK);
            resp.headers()
                .get(&X_REQUEST_ID)
                .and_then(|v| v.to_str().ok())
                .unwrap()
                .to_string()
        }));
    }

    let mut ids = std::collections::HashSet::new();
    for handle in handles {
        let id = handle.await.unwrap();
        // Each ID must be 36 chars long (UUID format)
        assert_eq!(id.len(), 36);
        // Each ID must be unique
        assert!(ids.insert(id), "duplicate request ID generated");
    }
    assert_eq!(ids.len(), 50);
}

#[tokio::test]
async fn tracing_context_preserved_across_async_yields() {
    let ctx = test_ctx();

    async fn yielding_handler(request_id: RequestId) -> String {
        let initial_id = request_id.to_string();
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;
        let final_id = request_id.to_string();
        assert_eq!(initial_id, final_id);
        final_id
    }

    let builder = Rume::builder(ctx).route("/yield", get(yielding_handler));
    let router = builder.build_router(test_ctx()).unwrap();

    let mut handles = Vec::new();
    for i in 0..20 {
        let router_clone = router.clone();
        let custom_id = format!("yield-trace-req-{i}");
        handles.push(tokio::spawn(async move {
            let app = TestApp::new(router_clone.with_state(test_ctx()));
            let req = axum::http::Request::builder()
                .uri("/yield")
                .header("x-request-id", &custom_id)
                .body(axum::body::Body::empty())
                .unwrap();
            let resp = app.request(req).await;
            assert_eq!(resp.status(), StatusCode::OK);
            let hdr_id = resp
                .headers()
                .get(&X_REQUEST_ID)
                .and_then(|v| v.to_str().ok())
                .unwrap()
                .to_string();
            let body = TestApp::body_text(resp).await;
            assert_eq!(hdr_id, custom_id);
            assert_eq!(body, custom_id);
        }));
    }

    for handle in handles {
        handle.await.unwrap();
    }
}

#[tokio::test]
async fn test_w3c_trace_context_automatic_propagation_across_http_outbox_jobs_and_worker() {
    use rume::app::TraceContext;
    use rume::jobs::{Job, JobHandler, JobQueue, JobRegistry, QueuedJob};
    use rume::outbox::OutboxMessage;
    use rume::testing::jobs::TestJobQueue;
    use serde::{Deserialize, Serialize};
    use std::sync::Arc;

    #[derive(Debug, Clone, Serialize, Deserialize)]
    struct TraceJob {
        data: String,
    }

    impl Job for TraceJob {
        const NAME: &'static str = "test.trace_job";
    }

    struct TraceJobHandler {
        received_trace_id: Arc<tokio::sync::Mutex<Option<String>>>,
        received_span_id: Arc<tokio::sync::Mutex<Option<String>>>,
    }

    impl JobHandler<TraceJob> for TraceJobHandler {
        fn handle<'a>(
            &'a self,
            _ctx: &'a rume::jobs::JobContext,
            _job: TraceJob,
        ) -> rume::jobs::BoxFuture<'a, Result<(), rume::jobs::JobError>> {
            Box::pin(async move {
                if let Some(tc) = TraceContext::current() {
                    let mut tid = self.received_trace_id.lock().await;
                    *tid = Some(tc.trace_id().to_string());
                    let mut sid = self.received_span_id.lock().await;
                    *sid = Some(tc.span_id().to_string());
                }
                Ok(())
            })
        }
    }

    let captured_outbox_tc = Arc::new(tokio::sync::Mutex::new(None));
    let captured_job_tc = Arc::new(tokio::sync::Mutex::new(None));
    let memory_queue = Arc::new(TestJobQueue::new());

    let outbox_tc_clone = Arc::clone(&captured_outbox_tc);
    let job_tc_clone = Arc::clone(&captured_job_tc);
    let queue_clone = Arc::clone(&memory_queue);

    let ctx = test_ctx();
    let builder = Rume::builder(ctx).route(
        "/trigger-trace",
        get(move || {
            let outbox_tc_clone = Arc::clone(&outbox_tc_clone);
            let job_tc_clone = Arc::clone(&job_tc_clone);
            let queue = Arc::clone(&queue_clone);
            async move {
                // Inside HTTP handler, TraceContext::current() must be available
                let current_tc =
                    TraceContext::current().expect("TraceContext must be present in HTTP handler");
                assert_eq!(current_tc.trace_id(), "4bf92f3577b34da6a3ce929d0e0e4736");

                // OutboxMessage::new automatically inherits current trace_context
                let msg = OutboxMessage::new("msg-1", "user.created", vec![]);
                let mut outbox_guard = outbox_tc_clone.lock().await;
                *outbox_guard = msg.trace_context.clone();

                // Job enqueue automatically inherits current trace_context
                let job_id = queue
                    .enqueue(TraceJob {
                        data: "hello".into(),
                    })
                    .await
                    .expect("enqueue must succeed");

                let dequeued = queue
                    .dequeue_for("test-worker")
                    .await
                    .expect("dequeue must succeed")
                    .expect("job must be available");

                let mut job_guard = job_tc_clone.lock().await;
                *job_guard = dequeued.trace_context.clone();

                format!("ok:{job_id}")
            }
        }),
    );
    let router = builder.build_router(test_ctx()).unwrap();
    let app = TestApp::new(router.with_state(test_ctx()));

    let incoming_trace_id = "4bf92f3577b34da6a3ce929d0e0e4736";
    let incoming_span_id = "00f067aa0ba902b7";
    let incoming_traceparent = format!("00-{incoming_trace_id}-{incoming_span_id}-01");

    let req = axum::http::Request::builder()
        .uri("/trigger-trace")
        .header("traceparent", &incoming_traceparent)
        .body(axum::body::Body::empty())
        .unwrap();

    let resp = app.request(req).await;
    assert_eq!(resp.status(), StatusCode::OK);

    // 1. Response has matching traceparent
    let resp_traceparent = resp
        .headers()
        .get("traceparent")
        .and_then(|v| v.to_str().ok())
        .expect("missing traceparent in response");
    assert_eq!(resp_traceparent, incoming_traceparent);

    // 2. Outbox message inherited exact incoming traceparent
    let outbox_tc = captured_outbox_tc
        .lock()
        .await
        .clone()
        .expect("outbox trace context captured");
    assert_eq!(outbox_tc, incoming_traceparent);

    // 3. Queued job inherited exact incoming traceparent
    let job_tc = captured_job_tc
        .lock()
        .await
        .clone()
        .expect("job trace context captured");
    assert_eq!(job_tc, incoming_traceparent);

    // 4. Now dispatch the queued job through JobRegistry and verify child span creation
    let job_handler_tid = Arc::new(tokio::sync::Mutex::new(None));
    let job_handler_sid = Arc::new(tokio::sync::Mutex::new(None));

    let mut registry = JobRegistry::new();
    registry.register(TraceJobHandler {
        received_trace_id: Arc::clone(&job_handler_tid),
        received_span_id: Arc::clone(&job_handler_sid),
    });

    let queued_job = QueuedJob {
        id: "job-123".into(),
        name: TraceJob::NAME.to_string(),
        payload: serde_json::to_vec(&TraceJob {
            data: "test".into(),
        })
        .unwrap(),
        attempt: 1,
        retry_policy: Default::default(),
        timeout: None,
        idempotency_key: None,
        tenant_id: None,
        assigned_worker: Some("worker-1".into()),
        lease_token: Some("token-1".into()),
        available_at: std::time::Instant::now(),
        lease_duration: std::time::Duration::from_secs(60),
        trace_context: Some(incoming_traceparent.clone()),
    };

    registry
        .dispatch_with_cancellation(&queued_job, None)
        .await
        .expect("dispatch succeeds");

    let worker_tid = job_handler_tid
        .lock()
        .await
        .clone()
        .expect("worker received trace id");
    let worker_sid = job_handler_sid
        .lock()
        .await
        .clone()
        .expect("worker received span id");

    // Worker execution preserved trace_id
    assert_eq!(worker_tid, incoming_trace_id);
    // Worker execution generated a new unique child span_id
    assert_ne!(worker_sid, incoming_span_id);
    assert_eq!(worker_sid.len(), 16);
}
