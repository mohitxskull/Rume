use axum::http::StatusCode;
use rume::openapi::{OpenApiInfo, generate_openapi};
use rume::prelude::*;

#[tokio::test]
async fn openapi_generation_from_route_registry() {
    let mut registry = RouteRegistry::new();

    registry.register(RouteInfo {
        method: axum::http::Method::GET,
        path: "/users/{id}".into(),
        name: Some("users.show".into()),
        params: vec!["id".into()],
    });

    registry.register(RouteInfo {
        method: axum::http::Method::POST,
        path: "/users".into(),
        name: Some("users.create".into()),
        params: vec![],
    });

    let info = OpenApiInfo::new("User Service", "1.0.0")
        .with_description("Production user management API");

    let spec = generate_openapi(&registry, info);

    assert_eq!(spec.openapi, "3.1.0");
    assert_eq!(spec.info.title, "User Service");
    assert_eq!(spec.info.version, "1.0.0");
    assert_eq!(
        spec.info.description.as_deref(),
        Some("Production user management API")
    );

    // Verify /users/{id}
    let user_path = spec.paths.get("/users/{id}").expect("/users/{id} path");
    let get_op = user_path.get.as_ref().expect("GET operation");
    assert_eq!(get_op.operation_id.as_deref(), Some("users.show"));
    assert_eq!(get_op.parameters.len(), 1);
    assert_eq!(get_op.parameters[0].name, "id");
    assert_eq!(get_op.parameters[0].schema.r#type, "integer");
    assert!(get_op.responses.contains_key("200"));
    assert!(get_op.responses.contains_key("400"));
    assert!(get_op.responses.contains_key("422"));

    // Verify /users
    let users_path = spec.paths.get("/users").expect("/users path");
    let post_op = users_path.post.as_ref().expect("POST operation");
    assert_eq!(post_op.operation_id.as_deref(), Some("users.create"));
    assert!(post_op.parameters.is_empty());

    // JSON export
    let json_str = spec.to_json_string().unwrap();
    assert!(json_str.contains("\"openapi\": \"3.1.0\""));
    assert!(json_str.contains("\"title\": \"User Service\""));
}

#[tokio::test]
async fn openapi_endpoint_mounted_on_router() {
    let info = OpenApiInfo::new("E-Commerce API", "2.1.0");

    let router = rume::routing::Router::new()
        .get("/products", || async { "list products" })
        .get("/products/{uuid}", || async { "show product" })
        .openapi("/openapi.json", info);

    let app = TestApp::new(router);

    let res = app.get("/openapi.json").await;
    assert_eq!(res.status(), StatusCode::OK);

    let body_text = TestApp::body_text(res).await;
    let doc: serde_json::Value = serde_json::from_str(&body_text).unwrap();

    assert_eq!(doc["openapi"], "3.1.0");
    assert_eq!(doc["info"]["title"], "E-Commerce API");
    assert_eq!(doc["info"]["version"], "2.1.0");

    assert!(doc["paths"]["/products"].is_object());
    assert!(doc["paths"]["/products/{uuid}"].is_object());
    assert_eq!(
        doc["paths"]["/products/{uuid}"]["get"]["parameters"][0]["schema"]["format"],
        "uuid"
    );
}
