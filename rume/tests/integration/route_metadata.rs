//! Integration tests for Milestone 1: Canonical Route Registration and Metadata Reflection.
//!
//! Verifies:
//! - R1: Route methods, names, parameters, and paths are atomically registered when routes are mounted.
//! - R3: Metadata matches mounted routes across `.get()`, `.post()`, `.put()`, `.delete()`,
//!   `.route_spec()`, `.resource()`, `.nest()`, and `.merge()`.
//! - R4: Axum interoperability contract (`into_inner()`, `into_parts()`, `merge_axum()`, `From`).
//! - HTTP dispatch matches router metadata at runtime.

use axum::http::{Method, StatusCode};
use axum::routing::{delete, get, post, put};
use rume::prelude::*;
use rume::routing::{Matcher, Resource, RouteInfo, RouteRegistry, RouteSpec, Router};

async fn dummy_handler() -> &'static str {
    "ok"
}

async fn echo_two_params(
    axum::extract::Path((org, user)): axum::extract::Path<(String, String)>,
) -> String {
    format!("{org}:{user}")
}

#[tokio::test]
async fn router_method_helpers_register_metadata_and_dispatch() {
    let router = Router::new()
        .get("/items", dummy_handler)
        .post("/items", dummy_handler)
        .put("/items/{id}", dummy_handler)
        .patch("/items/{id}", dummy_handler)
        .delete("/items/{id}", dummy_handler);

    let (inner_router, registry) = router.into_parts();
    let routes = registry.routes();

    assert_eq!(routes.len(), 5);

    // Verify GET
    assert_eq!(routes[0].method, Method::GET);
    assert_eq!(routes[0].path, "/items");
    assert!(routes[0].params.is_empty());

    // Verify POST
    assert_eq!(routes[1].method, Method::POST);
    assert_eq!(routes[1].path, "/items");
    assert!(routes[1].params.is_empty());

    // Verify PUT
    assert_eq!(routes[2].method, Method::PUT);
    assert_eq!(routes[2].path, "/items/{id}");
    assert_eq!(routes[2].params, vec!["id".to_string()]);

    // Verify PATCH
    assert_eq!(routes[3].method, Method::PATCH);
    assert_eq!(routes[3].path, "/items/{id}");
    assert_eq!(routes[3].params, vec!["id".to_string()]);

    // Verify DELETE
    assert_eq!(routes[4].method, Method::DELETE);
    assert_eq!(routes[4].path, "/items/{id}");
    assert_eq!(routes[4].params, vec!["id".to_string()]);

    // Verify HTTP requests dispatch correctly through axum
    let app = TestApp::new(inner_router);
    let res = app.get("/items").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(TestApp::body_text(res).await, "ok");
}

#[tokio::test]
async fn route_spec_registration_with_validation_and_metadata() {
    let spec = RouteSpec::get("/orgs/{org_id}/members/{user_id}")
        .where_param("org_id", Matcher::Slug)
        .where_param("user_id", Matcher::Number)
        .r#as("orgs.members.show");

    let router = Router::new().route_spec(spec, echo_two_params);
    let routes = router.routes();

    assert_eq!(routes.len(), 1);
    let r = &routes[0];
    assert_eq!(r.method, Method::GET);
    assert_eq!(r.path, "/orgs/{org_id}/members/{user_id}");
    assert_eq!(r.name.as_deref(), Some("orgs.members.show"));
    assert_eq!(r.params, vec!["org_id".to_string(), "user_id".to_string()]);

    let app = TestApp::new(router.into_inner());

    // Valid call
    let res = app.get("/orgs/acme/members/123").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(TestApp::body_text(res).await, "acme:123");

    // Invalid user_id (not numeric) -> fails closed with 400 Problem Details
    let res_invalid = app.get("/orgs/acme/members/abc").await;
    assert_eq!(res_invalid.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn resource_registration_registers_routes_and_metadata() {
    let resource = Resource::new("articles")
        .index(get(dummy_handler))
        .show(get(dummy_handler))
        .create(post(dummy_handler))
        .update(put(dummy_handler))
        .destroy(delete(dummy_handler));

    let router = Router::new().resource(resource);
    let routes = router.routes();

    assert_eq!(routes.len(), 5);

    // 1. GET /articles (articles.index)
    assert_eq!(routes[0].method, Method::GET);
    assert_eq!(routes[0].path, "/articles");
    assert_eq!(routes[0].name.as_deref(), Some("articles.index"));
    assert!(routes[0].params.is_empty());

    // 2. GET /articles/{id} (articles.show)
    assert_eq!(routes[1].method, Method::GET);
    assert_eq!(routes[1].path, "/articles/{id}");
    assert_eq!(routes[1].name.as_deref(), Some("articles.show"));
    assert_eq!(routes[1].params, vec!["id".to_string()]);

    // 3. POST /articles (articles.create)
    assert_eq!(routes[2].method, Method::POST);
    assert_eq!(routes[2].path, "/articles");
    assert_eq!(routes[2].name.as_deref(), Some("articles.create"));
    assert!(routes[2].params.is_empty());

    // 4. PUT /articles/{id} (articles.update)
    assert_eq!(routes[3].method, Method::PUT);
    assert_eq!(routes[3].path, "/articles/{id}");
    assert_eq!(routes[3].name.as_deref(), Some("articles.update"));
    assert_eq!(routes[3].params, vec!["id".to_string()]);

    // 5. DELETE /articles/{id} (articles.destroy)
    assert_eq!(routes[4].method, Method::DELETE);
    assert_eq!(routes[4].path, "/articles/{id}");
    assert_eq!(routes[4].name.as_deref(), Some("articles.destroy"));
    assert_eq!(routes[4].params, vec!["id".to_string()]);

    // Verify HTTP dispatch
    let app = TestApp::new(router.into_inner());
    assert_eq!(app.get("/articles").await.status(), StatusCode::OK);
    assert_eq!(app.get("/articles/42").await.status(), StatusCode::OK);
}

#[tokio::test]
async fn nested_routers_prefix_both_axum_and_registry() {
    let api_v1 = Router::new()
        .get("/users", dummy_handler)
        .get("/users/{id}", dummy_handler);

    let main_router = Router::new()
        .get("/health", dummy_handler)
        .nest("/api/v1", api_v1);

    let routes = main_router.routes();
    assert_eq!(routes.len(), 3);

    assert_eq!(routes[0].path, "/health");
    assert_eq!(routes[1].path, "/api/v1/users");
    assert_eq!(routes[2].path, "/api/v1/users/{id}");
    assert_eq!(routes[2].params, vec!["id".to_string()]);

    let app = TestApp::new(main_router.into_inner());
    assert_eq!(app.get("/health").await.status(), StatusCode::OK);
    assert_eq!(app.get("/api/v1/users").await.status(), StatusCode::OK);
    assert_eq!(app.get("/api/v1/users/99").await.status(), StatusCode::OK);
}

#[tokio::test]
async fn merged_routers_combine_axum_and_registries() {
    let r1 = Router::new().get("/alpha", dummy_handler);
    let r2 = Router::new().get("/beta", dummy_handler);

    let combined = r1.merge(r2);
    let routes = combined.routes();

    assert_eq!(routes.len(), 2);
    assert_eq!(routes[0].path, "/alpha");
    assert_eq!(routes[1].path, "/beta");

    let app = TestApp::new(combined.into_inner());
    assert_eq!(app.get("/alpha").await.status(), StatusCode::OK);
    assert_eq!(app.get("/beta").await.status(), StatusCode::OK);
}

#[tokio::test]
async fn axum_interoperability_and_conversions() {
    // 1. From axum::Router
    let axum_r = axum::Router::new().route("/axum-raw", get(|| async { "raw" }));
    let mut rume_r = Router::from(axum_r);
    assert_eq!(rume_r.routes().len(), 0);

    // 2. merge_axum preserves existing metadata and mounts axum route
    rume_r = rume_r.get("/rume-native", dummy_handler);
    assert_eq!(rume_r.routes().len(), 1);

    let external_axum = axum::Router::new().route("/external", get(|| async { "ext" }));
    let merged = rume_r.merge_axum(external_axum);
    assert_eq!(merged.routes().len(), 1);

    let app = TestApp::new(merged.into_inner());
    assert_eq!(app.get("/axum-raw").await.status(), StatusCode::OK);
    assert_eq!(app.get("/rume-native").await.status(), StatusCode::OK);
    assert_eq!(app.get("/external").await.status(), StatusCode::OK);
}

#[tokio::test]
async fn route_registry_serialization_round_trip() {
    let mut reg = RouteRegistry::new();
    reg.register(RouteInfo {
        method: Method::POST,
        path: "/webhooks/{id}".to_string(),
        name: Some("webhooks.receive".to_string()),
        params: vec!["id".to_string()],
        matchers: vec![],
        request_schema: None,
    });

    let json = serde_json::to_string(&reg).expect("failed to serialize RouteRegistry");
    let deserialized: RouteRegistry =
        serde_json::from_str(&json).expect("failed to deserialize RouteRegistry");

    assert_eq!(deserialized.routes().len(), 1);
    let route = &deserialized.routes()[0];
    assert_eq!(route.method, Method::POST);
    assert_eq!(route.path, "/webhooks/{id}");
    assert_eq!(route.name.as_deref(), Some("webhooks.receive"));
    assert_eq!(route.params, vec!["id".to_string()]);
}

#[tokio::test]
async fn fallback_and_service_routing() {
    let router = Router::new()
        .get("/hello", dummy_handler)
        .fallback(|| async { (StatusCode::NOT_FOUND, "custom 404") });

    let app = TestApp::new(router.into_inner());

    let res = app.get("/hello").await;
    assert_eq!(res.status(), StatusCode::OK);

    let res404 = app.get("/unmatched-path").await;
    assert_eq!(res404.status(), StatusCode::NOT_FOUND);
    assert_eq!(TestApp::body_text(res404).await, "custom 404");
}
