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
    assert_eq!(json.get("code").and_then(|v| v.as_str()), Some("INVALID_INPUT"));
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
