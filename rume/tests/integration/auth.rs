use axum::Json;
use axum::http::StatusCode;
use axum::routing::{get, post};
use rume::auth::*;
use rume::error::AppError;
use rume::prelude::*;

async fn start_session_store() -> InMemorySessionStore {
    InMemorySessionStore::new()
}

async fn get_me(Authenticated(user): Authenticated<AuthUser>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "id": user.id,
        "roles": user.roles,
        "permissions": user.permissions,
    }))
}

async fn optional_profile(OptionalAuth(user): OptionalAuth<AuthUser>) -> Json<serde_json::Value> {
    match user {
        Some(u) => Json(serde_json::json!({ "authenticated": true, "user_id": u.id })),
        None => Json(serde_json::json!({ "authenticated": false, "user_id": null })),
    }
}

async fn admin_only_handler(
    Authenticated(user): Authenticated<AuthUser>,
) -> Result<Json<serde_json::Value>, AppError> {
    authorize(&user, &(), RequireRole("admin"))?;
    Ok(Json(serde_json::json!({ "secret": "super_admin_data" })))
}

async fn billing_write_handler(
    Authenticated(user): Authenticated<AuthUser>,
) -> Result<Json<serde_json::Value>, AppError> {
    authorize(&user, &(), RequirePermission("billing:write"))?;
    Ok(Json(serde_json::json!({ "status": "invoice_created" })))
}

#[tokio::test]
async fn bearer_token_authentication_flow() {
    let bearer = BearerTokenScheme::new(|token| async move {
        if token == "secret_token_123" {
            Ok(Some(
                AuthUser::new("user_alice")
                    .with_role("admin")
                    .with_permission("billing:write"),
            ))
        } else {
            Ok(None)
        }
    });

    let router = rume::routing::Router::new()
        .route("/me", get(get_me))
        .route("/optional", get(optional_profile))
        .layer(AuthLayer::required().scheme(bearer.clone()));

    let app = TestApp::new(router);

    // 1. Missing Authorization header -> 401
    let res = app.get("/me").await;
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    let val: serde_json::Value = serde_json::from_str(&TestApp::body_text(res).await).unwrap();
    assert_eq!(val["status"], 401);

    // 2. Invalid Bearer token -> 401
    let req = axum::http::Request::builder()
        .uri("/me")
        .header(axum::http::header::AUTHORIZATION, "Bearer wrong_token")
        .body(axum::body::Body::empty())
        .unwrap();
    let res = app.request(req).await;
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    // 3. Valid Bearer token -> 200 OK
    let req = axum::http::Request::builder()
        .uri("/me")
        .header(axum::http::header::AUTHORIZATION, "Bearer secret_token_123")
        .body(axum::body::Body::empty())
        .unwrap();
    let res = app.request(req).await;
    assert_eq!(res.status(), StatusCode::OK);
    let val: serde_json::Value = serde_json::from_str(&TestApp::body_text(res).await).unwrap();
    assert_eq!(val["id"], "user_alice");
    assert_eq!(val["roles"][0], "admin");
}

#[tokio::test]
async fn api_key_authentication_flow() {
    let api_key = ApiKeyScheme::new().with_header("x-service-key").add_key(
        "service_key_999",
        AuthUser::new("svc_billing").with_role("service"),
    );

    let router = rume::routing::Router::new()
        .route("/me", get(get_me))
        .layer(AuthLayer::required().scheme(api_key));

    let app = TestApp::new(router);

    // Missing key -> 401
    let res = app.get("/me").await;
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    // Invalid key -> 401
    let req = axum::http::Request::builder()
        .uri("/me")
        .header("x-service-key", "wrong_key")
        .body(axum::body::Body::empty())
        .unwrap();
    let res = app.request(req).await;
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    // Valid key -> 200
    let req = axum::http::Request::builder()
        .uri("/me")
        .header("x-service-key", "service_key_999")
        .body(axum::body::Body::empty())
        .unwrap();
    let res = app.request(req).await;
    assert_eq!(res.status(), StatusCode::OK);
    let val: serde_json::Value = serde_json::from_str(&TestApp::body_text(res).await).unwrap();
    assert_eq!(val["id"], "svc_billing");
    assert_eq!(val["roles"][0], "service");
}

#[tokio::test]
async fn session_cookie_lifecycle_and_expiration() {
    let store = start_session_store().await;

    // Create session for Bob (expires in 3600 seconds)
    let session = Session::new("sess_bob_active", "user_bob", 3600)
        .with_role("editor")
        .with_permission("articles:publish");
    store.save(session).await.unwrap();

    // Create expired session (0 TTL)
    let expired_session = Session::new("sess_bob_expired", "user_bob", 0);
    store.save(expired_session).await.unwrap();

    let session_scheme = SessionScheme::new(store.clone());
    let router = rume::routing::Router::new()
        .route("/me", get(get_me))
        .layer(AuthLayer::required().scheme(session_scheme));

    let app = TestApp::new(router);

    // 1. Valid session cookie -> 200 OK
    let req = axum::http::Request::builder()
        .uri("/me")
        .header(
            axum::http::header::COOKIE,
            "rume_session=sess_bob_active; path=/",
        )
        .body(axum::body::Body::empty())
        .unwrap();
    let res = app.request(req).await;
    assert_eq!(res.status(), StatusCode::OK);
    let val: serde_json::Value = serde_json::from_str(&TestApp::body_text(res).await).unwrap();
    assert_eq!(val["id"], "user_bob");
    assert_eq!(val["roles"][0], "editor");

    // 2. Expired session cookie -> 401
    let req = axum::http::Request::builder()
        .uri("/me")
        .header(axum::http::header::COOKIE, "rume_session=sess_bob_expired")
        .body(axum::body::Body::empty())
        .unwrap();
    let res = app.request(req).await;
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    // 3. Destroy session -> now returns 401
    store.destroy("sess_bob_active").await.unwrap();
    let req = axum::http::Request::builder()
        .uri("/me")
        .header(axum::http::header::COOKIE, "rume_session=sess_bob_active")
        .body(axum::body::Body::empty())
        .unwrap();
    let res = app.request(req).await;
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn optional_auth_guest_and_authenticated() {
    let api_key = ApiKeyScheme::new().add_key("key_carol", AuthUser::new("user_carol"));

    let router = rume::routing::Router::new()
        .route("/optional", get(optional_profile))
        .layer(AuthLayer::optional().scheme(api_key));

    let app = TestApp::new(router);

    // Guest request without credentials -> 200 with authenticated=false
    let res = app.get("/optional").await;
    assert_eq!(res.status(), StatusCode::OK);
    let val: serde_json::Value = serde_json::from_str(&TestApp::body_text(res).await).unwrap();
    assert_eq!(val["authenticated"], false);
    assert!(val["user_id"].is_null());

    // Authenticated request with credentials -> 200 with authenticated=true
    let req = axum::http::Request::builder()
        .uri("/optional")
        .header("x-api-key", "key_carol")
        .body(axum::body::Body::empty())
        .unwrap();
    let res = app.request(req).await;
    assert_eq!(res.status(), StatusCode::OK);
    let val: serde_json::Value = serde_json::from_str(&TestApp::body_text(res).await).unwrap();
    assert_eq!(val["authenticated"], true);
    assert_eq!(val["user_id"], "user_carol");
}

#[tokio::test]
async fn optional_auth_succeeds_as_guest_when_session_cookie_expired() {
    let store = start_session_store().await;
    let session_scheme = SessionScheme::new(store);
    let router = rume::routing::Router::new()
        .route("/optional", get(optional_profile))
        .layer(AuthLayer::optional().scheme(session_scheme));

    let app = TestApp::new(router);

    let req = axum::http::Request::builder()
        .uri("/optional")
        .header("cookie", "rume_session=expired_or_invalid_token")
        .body(axum::body::Body::empty())
        .unwrap();

    let res = app.request(req).await;
    assert_eq!(res.status(), StatusCode::OK);
    let val: serde_json::Value = serde_json::from_str(&TestApp::body_text(res).await).unwrap();
    assert_eq!(val["authenticated"], false);
    assert!(val["user_id"].is_null());
}

#[tokio::test]
async fn declarative_authorization_policies() {
    let api_key = ApiKeyScheme::new()
        .add_key(
            "admin_key",
            AuthUser::new("admin_user")
                .with_role("admin")
                .with_permission("billing:write"),
        )
        .add_key(
            "viewer_key",
            AuthUser::new("viewer_user")
                .with_role("viewer")
                .with_permission("billing:read"),
        );

    let router = rume::routing::Router::new()
        .route("/admin", get(admin_only_handler))
        .route("/billing", post(billing_write_handler))
        .layer(AuthLayer::required().scheme(api_key));

    let app = TestApp::new(router);

    // 1. Viewer accesses /admin -> 403 Forbidden
    let req = axum::http::Request::builder()
        .uri("/admin")
        .header("x-api-key", "viewer_key")
        .body(axum::body::Body::empty())
        .unwrap();
    let res = app.request(req).await;
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    let val: serde_json::Value = serde_json::from_str(&TestApp::body_text(res).await).unwrap();
    assert_eq!(val["status"], 403);
    assert_eq!(val["code"], "FORBIDDEN");

    // 2. Admin accesses /admin -> 200 OK
    let req = axum::http::Request::builder()
        .uri("/admin")
        .header("x-api-key", "admin_key")
        .body(axum::body::Body::empty())
        .unwrap();
    let res = app.request(req).await;
    assert_eq!(res.status(), StatusCode::OK);

    // 3. Viewer attempts billing:write -> 403 Forbidden
    let req = axum::http::Request::builder()
        .method("POST")
        .uri("/billing")
        .header("x-api-key", "viewer_key")
        .body(axum::body::Body::empty())
        .unwrap();
    let res = app.request(req).await;
    assert_eq!(res.status(), StatusCode::FORBIDDEN);

    // 4. Admin performs billing:write -> 200 OK
    let req = axum::http::Request::builder()
        .method("POST")
        .uri("/billing")
        .header("x-api-key", "admin_key")
        .body(axum::body::Body::empty())
        .unwrap();
    let res = app.request(req).await;
    assert_eq!(res.status(), StatusCode::OK);
}

#[tokio::test]
async fn policy_layer_guards_router_branch() {
    let api_key = ApiKeyScheme::new()
        .add_key("admin_token", AuthUser::new("boss").with_role("superadmin"))
        .add_key("guest_token", AuthUser::new("guest").with_role("guest"));

    // Guard an entire branch with PolicyLayer(RequireRole("superadmin"))
    let guarded_branch = rume::routing::Router::new()
        .route("/vip", get(|| async { "VIP Access Granted" }))
        .layer(PolicyLayer::new(RequireRole("superadmin")))
        .layer(AuthLayer::required().scheme(api_key));

    let app = TestApp::new(guarded_branch);

    // Guest denied at middleware level -> 403
    let req = axum::http::Request::builder()
        .uri("/vip")
        .header("x-api-key", "guest_token")
        .body(axum::body::Body::empty())
        .unwrap();
    let res = app.request(req).await;
    assert_eq!(res.status(), StatusCode::FORBIDDEN);

    // Superadmin permitted -> 200
    let req = axum::http::Request::builder()
        .uri("/vip")
        .header("x-api-key", "admin_token")
        .body(axum::body::Body::empty())
        .unwrap();
    let res = app.request(req).await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(TestApp::body_text(res).await, "VIP Access Granted");
}

#[tokio::test]
async fn test_auth_provider_error_maps_to_500() {
    let err = AuthError::ProviderError("downstream OIDC timeout".into());
    let app_err = AppError::from(err);
    assert_eq!(app_err.status_code(), StatusCode::INTERNAL_SERVER_ERROR);
}

#[tokio::test]
async fn test_stale_session_cookie_falls_back_to_bearer_token() {
    let session_store = start_session_store().await;
    let session_scheme = SessionScheme::new(session_store);

    let bearer_scheme = BearerTokenScheme::new(|token| async move {
        if token == "valid_bearer_token" {
            Ok(Some(AuthUser::new("user_fallback")))
        } else {
            Ok(None)
        }
    });

    let router = rume::routing::Router::new()
        .route("/profile", get(get_me))
        .layer(
            AuthLayer::required()
                .scheme(session_scheme)
                .scheme(bearer_scheme),
        );

    let app = TestApp::new(router);

    // Request carries stale session cookie AND valid bearer token
    let req = axum::http::Request::builder()
        .uri("/profile")
        .header("cookie", "rume_session=expired_session_cookie_abc")
        .header("authorization", "Bearer valid_bearer_token")
        .body(axum::body::Body::empty())
        .unwrap();

    let res = app.request(req).await;
    assert_eq!(res.status(), StatusCode::OK);
    let val: serde_json::Value = serde_json::from_str(&TestApp::body_text(res).await).unwrap();
    assert_eq!(val["id"], "user_fallback");
}
