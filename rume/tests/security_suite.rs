use axum::http::{Method, Request, StatusCode};
use axum::routing::{get, post};
use rume::prelude::*;

#[tokio::test]
async fn test_default_security_headers() {
    let app = Router::new()
        .route("/hello", get(|| async { "Hello World" }))
        .layer(SecurityHeadersLayer::new());

    let test_app = TestApp::new(app);

    let req = Request::builder()
        .uri("/hello")
        .body(axum::body::Body::empty())
        .unwrap();

    let res = test_app.request(req).await;
    assert_eq!(res.status(), StatusCode::OK);

    let headers = res.headers();
    assert_eq!(
        headers.get("content-security-policy").unwrap(),
        "default-src 'self'"
    );
    assert_eq!(headers.get("x-frame-options").unwrap(), "DENY");
    assert_eq!(headers.get("x-content-type-options").unwrap(), "nosniff");
    assert_eq!(
        headers.get("strict-transport-security").unwrap(),
        "max-age=31536000; includeSubDomains"
    );
    assert_eq!(
        headers.get("referrer-policy").unwrap(),
        "strict-origin-when-cross-origin"
    );
    assert_eq!(
        headers.get("permissions-policy").unwrap(),
        "geolocation=(), camera=(), microphone=()"
    );
    assert_eq!(headers.get("x-xss-protection").unwrap(), "0");
}

#[tokio::test]
async fn test_custom_security_headers() {
    let config = SecurityHeadersConfig::new()
        .with_csp("default-src 'none'; img-src https:")
        .with_frame_options("SAMEORIGIN")
        .with_header("x-custom-security", "enforced");

    let app = Router::new()
        .route("/custom", get(|| async { "Custom Headers" }))
        .layer(SecurityHeadersLayer::with_config(config));

    let test_app = TestApp::new(app);

    let req = Request::builder()
        .uri("/custom")
        .body(axum::body::Body::empty())
        .unwrap();

    let res = test_app.request(req).await;
    assert_eq!(res.status(), StatusCode::OK);

    let headers = res.headers();
    assert_eq!(
        headers.get("content-security-policy").unwrap(),
        "default-src 'none'; img-src https:"
    );
    assert_eq!(headers.get("x-frame-options").unwrap(), "SAMEORIGIN");
    assert_eq!(headers.get("x-custom-security").unwrap(), "enforced");
}

#[tokio::test]
async fn test_csrf_protection_lifecycle() {
    let csrf_config = CsrfConfig::new()
        .with_header("x-csrf-token")
        .with_cookie("csrf_token")
        .exempt("/api/webhook");

    let app = Router::new()
        .route("/data", get(|| async { "safe data" }))
        .route("/data", post(|| async { "updated" }))
        .route("/api/webhook", post(|| async { "webhook received" }))
        .layer(CsrfLayer::with_config(csrf_config));

    let test_app = TestApp::new(app);

    // 1. Safe GET request succeeds without CSRF tokens
    let get_req = Request::builder()
        .method(Method::GET)
        .uri("/data")
        .body(axum::body::Body::empty())
        .unwrap();
    let res = test_app.request(get_req).await;
    assert_eq!(res.status(), StatusCode::OK);

    // 2. Mutating POST without tokens fails with 403 CSRF_TOKEN_MISSING
    let post_req = Request::builder()
        .method(Method::POST)
        .uri("/data")
        .body(axum::body::Body::empty())
        .unwrap();
    let res = test_app.request(post_req).await;
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    let body: serde_json::Value = serde_json::from_str(&TestApp::body_text(res).await).unwrap();
    assert_eq!(body["code"], "CSRF_TOKEN_MISSING");

    // 3. Mutating POST with token in header but missing in cookie
    let post_req = Request::builder()
        .method(Method::POST)
        .uri("/data")
        .header("x-csrf-token", "token-12345")
        .body(axum::body::Body::empty())
        .unwrap();
    let res = test_app.request(post_req).await;
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    let body: serde_json::Value = serde_json::from_str(&TestApp::body_text(res).await).unwrap();
    assert_eq!(body["code"], "CSRF_TOKEN_MISSING");

    // 4. Mutating POST with mismatching tokens fails with 403 CSRF_TOKEN_MISMATCH
    let post_req = Request::builder()
        .method(Method::POST)
        .uri("/data")
        .header("x-csrf-token", "token-correct")
        .header("cookie", "csrf_token=token-incorrect")
        .body(axum::body::Body::empty())
        .unwrap();
    let res = test_app.request(post_req).await;
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    let body: serde_json::Value = serde_json::from_str(&TestApp::body_text(res).await).unwrap();
    assert_eq!(body["code"], "CSRF_TOKEN_MISMATCH");

    // 5. Mutating POST with matching tokens succeeds
    let post_req = Request::builder()
        .method(Method::POST)
        .uri("/data")
        .header("x-csrf-token", "secret-token-xyz")
        .header("cookie", "theme=dark; csrf_token=secret-token-xyz; lang=en")
        .body(axum::body::Body::empty())
        .unwrap();
    let res = test_app.request(post_req).await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(TestApp::body_text(res).await, "updated");

    // 6. Exempt path succeeds on POST without any CSRF tokens
    let webhook_req = Request::builder()
        .method(Method::POST)
        .uri("/api/webhook")
        .body(axum::body::Body::empty())
        .unwrap();
    let res = test_app.request(webhook_req).await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(TestApp::body_text(res).await, "webhook received");
}

#[tokio::test]
async fn test_audit_logging() {
    let logger = MemoryAuditLogger::new();

    let record = AuditRecord::new("user.login", "user", AuditStatus::Success)
        .with_actor("user-999")
        .with_target("user-999")
        .with_ip("192.168.1.100")
        .with_meta("device", "firefox");

    logger.record(record).await.unwrap();

    let failed_record = AuditRecord::new("payment.charge", "invoice", AuditStatus::Denied)
        .with_actor("user-999")
        .with_target("inv-42")
        .with_meta("reason", "insufficient_funds");

    logger.record(failed_record).await.unwrap();

    let records = logger.records();
    assert_eq!(records.len(), 2);

    assert_eq!(records[0].action, "user.login");
    assert_eq!(records[0].actor_id.as_deref(), Some("user-999"));
    assert_eq!(records[0].status, AuditStatus::Success);
    assert_eq!(records[0].metadata.get("device").unwrap(), "firefox");

    assert_eq!(records[1].action, "payment.charge");
    assert_eq!(records[1].status, AuditStatus::Denied);
    assert_eq!(
        records[1].metadata.get("reason").unwrap(),
        "insufficient_funds"
    );

    logger.clear();
    assert!(logger.records().is_empty());
}
