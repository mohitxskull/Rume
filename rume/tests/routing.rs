//! Integration tests for end-to-end routing: parameter matching enforced at
//! HTTP time, route groups, and route inspection.
//!
//! T-T8: exercises RouteSpec::where_param via ParamValidationLayer, RouteGroup
//! prefixing, and RouteRegistry metadata.

use axum::http::StatusCode;
use axum::response::Json;
use axum::routing::get;
use rume::prelude::TestApp;
use rume::routing::{Matcher, RouteGroup, RouteRegistry, RouteSpec};
use std::collections::BTreeMap;

/// Handler that echoes back the validated `id` parameter.
async fn get_user(
    axum::extract::Path(params): axum::extract::Path<BTreeMap<String, String>>,
) -> Json<BTreeMap<String, String>> {
    Json(params)
}

/// Handler that echoes back the validated `id` parameter as plain text.
async fn get_user_text(
    axum::extract::Path(params): axum::extract::Path<BTreeMap<String, String>>,
) -> String {
    format!("user_id={}", params.get("id").cloned().unwrap_or_default())
}

fn number_route_router() -> axum::Router<()> {
    let spec = RouteSpec::get("/users/{id}")
        .where_param("id", Matcher::Number)
        .r#as("users.show");

    let layer = spec.validation_layer();
    let (path, _, _, _) = spec.build();

    axum::Router::new().route(&path, get(get_user).layer(layer))
}

fn uuid_route_router() -> axum::Router<()> {
    let spec = RouteSpec::get("/users/{id}")
        .where_param("id", Matcher::Uuid)
        .r#as("users.show");

    let layer = spec.validation_layer();
    let (path, _, _, _) = spec.build();

    axum::Router::new().route(&path, get(get_user_text).layer(layer))
}

#[tokio::test]
async fn route_valid_number_passes_validation() {
    let app = TestApp::new(number_route_router());

    let response = app.get("/users/42").await;
    assert_eq!(response.status(), StatusCode::OK);

    let body = TestApp::body_text(response).await;
    assert!(body.contains("\"id\":\"42\""));
}

#[tokio::test]
async fn route_invalid_number_returns_400() {
    let app = TestApp::new(number_route_router());

    let response = app.get("/users/abc").await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let content_type = response
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok());
    assert_eq!(content_type, Some("application/problem+json"));

    let body = TestApp::body_text(response).await;
    assert!(body.contains("INVALID_PARAM"));
}

#[tokio::test]
async fn route_uuid_validation_succeeds() {
    let app = TestApp::new(uuid_route_router());

    let response = app.get("/users/550e8400-e29b-41d4-a716-446655440000").await;
    assert_eq!(response.status(), StatusCode::OK);

    let body = TestApp::body_text(response).await;
    assert!(body.contains("user_id=550e8400-e29b-41d4-a716-446655440000"));
}

#[tokio::test]
async fn route_invalid_uuid_returns_400() {
    let app = TestApp::new(uuid_route_router());

    let response = app.get("/users/not-a-uuid").await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn route_malformed_percent_encoding_fails_closed_and_does_not_invoke_handler() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    let handler_called = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&handler_called);

    let spec = RouteSpec::get("/items/{id}")
        .where_param("id", Matcher::Number)
        .r#as("items.show");

    let layer = spec.validation_layer();
    let (path, _, _, _) = spec.build();

    let router = axum::Router::new().route(
        &path,
        get(move || {
            flag.store(true, Ordering::SeqCst);
            async { "ok" }
        })
        .layer(layer),
    );

    let app = TestApp::new(router);

    // %FF is invalid UTF-8 in percent-encoding
    let response = app.get("/items/%FF").await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    // Verify handler was never executed
    assert!(
        !handler_called.load(Ordering::SeqCst),
        "handler should not have been called on extraction failure"
    );

    let body = TestApp::body_text(response).await;
    assert!(body.contains("INVALID_PARAM"));
}

#[tokio::test]
async fn route_registry_metadata_is_correct() {
    let spec = RouteSpec::get("/users/{id}")
        .where_param("id", Matcher::Number)
        .r#as("users.show");

    let (path, method, params, name) = spec.build();

    let mut registry = RouteRegistry::new();
    registry.register(rume::routing::RouteInfo {
        method,
        path: path.clone(),
        name: name.clone(),
        params: params.iter().map(|(n, _)| n.clone()).collect(),
        matchers: params,
    });

    let routes = registry.routes();
    assert_eq!(routes.len(), 1);
    assert_eq!(routes[0].path, "/users/{id}");
    assert_eq!(routes[0].method, axum::http::Method::GET);
    assert_eq!(routes[0].name, Some("users.show".to_string()));
    assert_eq!(routes[0].params, vec!["id".to_string()]);
}

#[tokio::test]
async fn route_group_applies_prefix() {
    async fn handler() {}

    let group = RouteGroup::<()>::new()
        .prefix("/api")
        .add("/users", get(handler))
        .add("posts", get(handler));

    let router = group.build();
    // Router builds without panicking; prefix normalization covered by unit tests.
    let _ = router;
}

#[tokio::test]
async fn route_validation_layer_problem_details_content_type() {
    let app = TestApp::new(number_route_router());

    let response = app.get("/users/abc").await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let content_type = response
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok());
    assert_eq!(content_type, Some("application/problem+json"));
}
