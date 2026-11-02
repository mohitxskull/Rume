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
