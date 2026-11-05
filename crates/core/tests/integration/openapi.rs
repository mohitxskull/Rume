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
        matchers: vec![],
        request_schema: None,
    });

    registry.register(RouteInfo {
        method: axum::http::Method::POST,
        path: "/users".into(),
        name: Some("users.create".into()),
        params: vec![],
        matchers: vec![],
        request_schema: None,
    });

    let info = OpenApiInfo::new("User Service", "1.0.0")
        .with_description("Production user management API");

    let spec = generate_openapi(&registry, info);

    insta::assert_json_snapshot!(spec, @r###"
    {
      "openapi": "3.1.0",
      "info": {
        "title": "User Service",
        "version": "1.0.0",
        "description": "Production user management API"
      },
      "paths": {
        "/users": {
          "post": {
            "operation_id": "users.create",
            "summary": "users.create",
            "responses": {
              "200": {
                "description": "Successful response"
              },
              "400": {
                "description": "Bad Request (RFC 9457 Problem Details)"
              },
              "422": {
                "description": "Validation Failed (RFC 9457 Problem Details)"
              }
            }
          }
        },
        "/users/{id}": {
          "get": {
            "operation_id": "users.show",
            "summary": "users.show",
            "parameters": [
              {
                "name": "id",
                "in": "path",
                "required": true,
                "schema": {
                  "type": "integer",
                  "format": "int64"
                }
              }
            ],
            "responses": {
              "200": {
                "description": "Successful response"
              },
              "400": {
                "description": "Bad Request (RFC 9457 Problem Details)"
              },
              "422": {
                "description": "Validation Failed (RFC 9457 Problem Details)"
              }
            }
          }
        }
      }
    }
    "###);

    // JSON export
    let json_str = spec.to_json_string().unwrap();
    assert!(json_str.contains("\"openapi\": \"3.1.0\""));
    assert!(json_str.contains("\"title\": \"User Service\""));
}

#[tokio::test]
async fn openapi_endpoint_mounted_on_router() {
    let info = OpenApiInfo::new("E-Commerce API", "2.1.0");

    let router: rume::routing::Router<()> = rume::routing::Router::new()
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

#[tokio::test]
async fn openapi_generation_with_explicit_parameter_matchers() {
    let spec =
        RouteSpec::get("/articles/{article_slug}").where_param("article_slug", Matcher::Slug);

    let router: rume::routing::Router<()> = rume::routing::Router::new()
        .route_spec(spec, || async { "article" })
        .route_spec(
            RouteSpec::get("/orders/{order_token}").where_param("order_token", Matcher::Uuid),
            || async { "order" },
        )
        .route_spec(
            RouteSpec::get("/items/{item_index}").where_param("item_index", Matcher::Number),
            || async { "item" },
        );

    let info = OpenApiInfo::new("Blog API", "1.0.0");
    let openapi_doc = router.openapi_spec(info);

    let paths = &openapi_doc.paths;

    // 1. article_slug matches Matcher::Slug -> string with regex pattern
    let slug_param = &paths["/articles/{article_slug}"]
        .get
        .as_ref()
        .unwrap()
        .parameters[0];
    assert_eq!(slug_param.name, "article_slug");
    assert_eq!(slug_param.schema.r#type, "string");
    assert_eq!(
        slug_param.schema.pattern.as_deref(),
        Some("^[a-zA-Z0-9][a-zA-Z0-9_-]{1,}[a-zA-Z0-9]$")
    );

    // 2. order_token matches Matcher::Uuid -> uuid format (even without 'uuid' in name)
    let uuid_param = &paths["/orders/{order_token}"]
        .get
        .as_ref()
        .unwrap()
        .parameters[0];
    assert_eq!(uuid_param.name, "order_token");
    assert_eq!(uuid_param.schema.format.as_deref(), Some("uuid"));

    // 3. item_index matches Matcher::Number -> integer format
    let num_param = &paths["/items/{item_index}"]
        .get
        .as_ref()
        .unwrap()
        .parameters[0];
    assert_eq!(num_param.name, "item_index");
    assert_eq!(num_param.schema.r#type, "integer");
}
