#![cfg(feature = "sqlite")]

use axum::http::{Method, Request, StatusCode};
use axum::routing::{get, post};
use rume::persistence::{DatabaseBackend, SqliteBackend};
use rume::prelude::*;
use rume::security::*;

#[tokio::test]
async fn test_default_security_headers() {
    let app = Router::new()
        .route("/hello", get(|| async { "Hello World" }))
        .layer(SecurityHeadersLayer::new());

    let test_app = TestApp::new(app);

    // 1. Plain HTTP request omits HSTS (RFC 6797 Section 7.2)
    let plain_req = Request::builder()
        .uri("/hello")
        .body(axum::body::Body::empty())
        .unwrap();

    let res = test_app.request(plain_req).await;
    assert_eq!(res.status(), StatusCode::OK);

    let headers = res.headers();
    assert_eq!(
        headers.get("content-security-policy").unwrap(),
        "default-src 'self'"
    );
    assert_eq!(headers.get("x-frame-options").unwrap(), "DENY");
    assert_eq!(headers.get("x-content-type-options").unwrap(), "nosniff");
    assert!(
        headers.get("strict-transport-security").is_none(),
        "HSTS must not be injected over non-secure transport"
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

    // 2. HTTPS / x-forwarded-proto: https request emits HSTS
    let secure_req = Request::builder()
        .uri("/hello")
        .header("x-forwarded-proto", "https")
        .body(axum::body::Body::empty())
        .unwrap();

    let res_secure = test_app.request(secure_req).await;
    assert_eq!(res_secure.status(), StatusCode::OK);
    assert_eq!(
        res_secure
            .headers()
            .get("strict-transport-security")
            .unwrap(),
        "max-age=31536000; includeSubDomains"
    );
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
        .allow_unauthenticated(true)
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
async fn test_csrf_signed_tokens_and_cookie_tossing_protection() {
    let secret = b"super-secret-hmac-key-for-csrf-protection";
    let csrf_config = CsrfConfig::new()
        .with_secret(*secret)
        .with_auto_inject_cookie(true)
        .with_same_site("Strict");

    let app = Router::new()
        .route("/form", get(|| async { "form view" }))
        .route("/submit", post(|| async { "form submitted" }))
        .layer(CsrfLayer::with_config(csrf_config.clone()));

    let test_app = TestApp::new(app);

    // 1. GET request auto-injects signed CSRF cookie
    let get_req = Request::builder()
        .method(Method::GET)
        .uri("/form")
        .body(axum::body::Body::empty())
        .unwrap();
    let res = test_app.request(get_req).await;
    assert_eq!(res.status(), StatusCode::OK);
    let set_cookie = res.headers().get("set-cookie").unwrap().to_str().unwrap();
    assert!(set_cookie.contains("csrf_token="));
    assert!(set_cookie.contains("SameSite=Strict"));

    // Extract the signed token from the cookie
    let token = set_cookie
        .split(';')
        .next()
        .unwrap()
        .strip_prefix("csrf_token=")
        .unwrap();

    // 2. Submitting with valid signed token in both header and cookie succeeds
    let post_req = Request::builder()
        .method(Method::POST)
        .uri("/submit")
        .header("x-csrf-token", token)
        .header("cookie", format!("csrf_token={token}"))
        .body(axum::body::Body::empty())
        .unwrap();
    let res = test_app.request(post_req).await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(TestApp::body_text(res).await, "form submitted");

    // 3. Subdomain cookie-tossing attack: attacker injects unsigned / forged cookie
    let tossed_req = Request::builder()
        .method(Method::POST)
        .uri("/submit")
        .header("x-csrf-token", "attacker_injected_nonce")
        .header("cookie", "csrf_token=attacker_injected_nonce")
        .body(axum::body::Body::empty())
        .unwrap();
    let res = test_app.request(tossed_req).await;
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    let body: serde_json::Value = serde_json::from_str(&TestApp::body_text(res).await).unwrap();
    assert_eq!(body["code"], "CSRF_TOKEN_INVALID");
}

#[tokio::test]
async fn test_audit_logging() {
    let backend = SqliteBackend::in_memory().await.unwrap();
    let logger = SqlAuditLogger::new(backend);
    logger.init_table().await.unwrap();

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

    assert_eq!(logger.count().await.unwrap(), 2);

    let action = logger
        .backend()
        .query_scalar("SELECT action FROM rume_audit_log WHERE actor_id = 'user-999' ORDER BY created_at ASC LIMIT 1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(action, "user.login");

    let status = logger
        .backend()
        .query_scalar("SELECT status FROM rume_audit_log WHERE resource = 'invoice'")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status, "denied");
}

#[tokio::test]
async fn test_audit_logging_sql_injection_resilience_and_id_preservation() {
    let backend = SqliteBackend::in_memory().await.unwrap();
    let logger = SqlAuditLogger::new(backend);
    logger.init_table().await.unwrap();

    let malicious_record = AuditRecord::new("user.inject", "account", AuditStatus::Failure)
        .with_actor("corp\\attacker\\' OR '1'='1; --")
        .with_target("tgt\\target\\")
        .with_ip("127.0.0.1\\")
        .with_meta("key", "val\\' OR '1'='1");
    let record_id = malicious_record.id.clone();

    logger
        .record(malicious_record)
        .await
        .expect("record with backslashes and quotes must succeed");
    assert_eq!(logger.count().await.unwrap(), 1);

    // Verify record.id was preserved in the database
    let saved_id = logger
        .backend()
        .query_scalar(&format!(
            "SELECT id FROM rume_audit_log WHERE id = '{record_id}'"
        ))
        .await
        .unwrap();
    assert_eq!(saved_id.as_deref(), Some(record_id.as_str()));
}

#[tokio::test]
async fn test_csrf_fails_closed_without_secret_by_default() {
    // Security M1: CsrfConfig::new() without secret or allow_unauthenticated fails closed
    let csrf_config = CsrfConfig::new()
        .with_header("x-csrf-token")
        .with_cookie("csrf_token");

    let app = Router::new()
        .route("/data", post(|| async { "updated" }))
        .layer(CsrfLayer::with_config(csrf_config));

    let test_app = TestApp::new(app);

    let post_req = Request::builder()
        .method(Method::POST)
        .uri("/data")
        .header("x-csrf-token", "arbitrary-token")
        .header("cookie", "csrf_token=arbitrary-token")
        .body(axum::body::Body::empty())
        .unwrap();

    let res = test_app.request(post_req).await;
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    let body: serde_json::Value = serde_json::from_str(&TestApp::body_text(res).await).unwrap();
    assert_eq!(body["code"], "CSRF_TOKEN_INVALID");
}

#[tokio::test]
async fn test_csrf_split_cookie_headers() {
    // Security m2: CsrfService inspects cookies across multiple RFC 7540 split Cookie headers
    let secret = b"super-secret-hmac-key-for-split-cookies";
    let csrf_config = CsrfConfig::new()
        .with_secret(secret.to_vec())
        .with_header("x-csrf-token")
        .with_cookie("csrf_token");

    let token = csrf_config.generate_token();

    let app = Router::new()
        .route("/data", post(|| async { "updated" }))
        .layer(CsrfLayer::with_config(csrf_config));

    let test_app = TestApp::new(app);

    // Request with two separate Cookie headers
    let post_req = Request::builder()
        .method(Method::POST)
        .uri("/data")
        .header("x-csrf-token", &token)
        .header("cookie", "session=xyz")
        .header("cookie", format!("csrf_token={token}"))
        .body(axum::body::Body::empty())
        .unwrap();

    let res = test_app.request(post_req).await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(TestApp::body_text(res).await, "updated");
}
