//! Optional async-graphql integration with W3C TraceContext and `AppError`
//! extensions for Rume.

#![forbid(unsafe_code)]

pub mod error;
pub mod router;
pub mod trace;

pub use error::app_error_to_graphql;
pub use router::{GraphQlConfig, graphql_routes, graphql_routes_from_builder};
pub use trace::RumeTraceExtension;

#[cfg(test)]
mod tests {
    use async_graphql::{EmptyMutation, EmptySubscription, Object, Schema};
    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode, header};
    use rume_foundation::error::AppError;
    use tower::ServiceExt;

    use super::*;

    struct QueryRoot;

    struct NestedNode;

    #[Object]
    impl NestedNode {
        async fn next(&self) -> NestedNode {
            NestedNode
        }

        async fn value(&self) -> &str {
            "leaf"
        }
    }

    #[Object]
    impl QueryRoot {
        async fn hello(&self) -> &str {
            "world"
        }

        async fn node(&self) -> NestedNode {
            NestedNode
        }

        async fn fail(&self) -> async_graphql::Result<&str> {
            Err(app_error_to_graphql(&AppError::not_found("item missing")))
        }
    }

    #[tokio::test]
    async fn graphql_router_executes_query_and_maps_app_error_extensions() {
        let schema = Schema::build(QueryRoot, EmptyMutation, EmptySubscription)
            .extension(RumeTraceExtension)
            .finish();

        let app = graphql_routes(schema, GraphQlConfig::default());

        // 1. Successful query
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/graphql")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"query":"{ hello }"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["data"]["hello"], "world");

        // 2. Erroring resolver maps AppError into GraphQL extensions
        let res = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/graphql")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"query":"{ fail }"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["errors"][0]["extensions"]["code"], "NOT_FOUND");
        assert_eq!(json["errors"][0]["extensions"]["status"], 404);
    }

    #[tokio::test]
    async fn graphql_routes_from_builder_enforces_max_depth_and_max_complexity() {
        let depth_config = GraphQlConfig::default()
            .with_max_depth(Some(2))
            .with_max_complexity(None);
        let depth_app = graphql_routes_from_builder(
            Schema::build(QueryRoot, EmptyMutation, EmptySubscription),
            depth_config,
        );

        // Depth 3 exceeds max_depth = 2
        let res = depth_app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/graphql")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"query":"{ node { next { value } } }"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(
            json["errors"].as_array().is_some_and(|e| !e.is_empty()),
            "query exceeding max_depth must be rejected: {json}"
        );

        // Depth 2 is within max_depth = 2
        let res = depth_app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/graphql")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"query":"{ node { value } }"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["data"]["node"]["value"], "leaf");

        // Complexity 3 exceeds max_complexity = 2
        let complexity_config = GraphQlConfig::default()
            .with_max_depth(None)
            .with_max_complexity(Some(2));
        let complexity_app = graphql_routes_from_builder(
            Schema::build(QueryRoot, EmptyMutation, EmptySubscription),
            complexity_config,
        );
        let res = complexity_app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/graphql")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"query":"{ a: hello b: hello c: hello }"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(
            json["errors"].as_array().is_some_and(|e| !e.is_empty()),
            "query exceeding max_complexity must be rejected: {json}"
        );
    }

    #[tokio::test]
    async fn graphql_forwards_extensions_disables_introspection_by_default_and_preserves_validation_details()
     {
        use async_graphql::Context;
        use rume_foundation::RequestId;
        use rume_foundation::error::ValidationErrorDetail;
        use rume_observability::TraceContext;
        use rume_security::auth::AuthUser;
        use rume_security::tenant::Tenant;

        struct ContextQuery;

        #[Object]
        impl ContextQuery {
            async fn whoami(&self, ctx: &Context<'_>) -> String {
                let req_id = ctx
                    .data_opt::<RequestId>()
                    .map(|r| r.as_str())
                    .unwrap_or("none");
                let trace_id = ctx
                    .data_opt::<TraceContext>()
                    .map(|t| t.trace_id().to_string())
                    .unwrap_or_else(|| "none".into());
                let user = ctx
                    .data_opt::<AuthUser>()
                    .map(|u| u.id.as_str())
                    .unwrap_or("none");
                let tenant = ctx
                    .data_opt::<Tenant>()
                    .map(|t| t.id.as_str())
                    .unwrap_or("none");
                format!("{req_id}|{trace_id}|{user}|{tenant}")
            }

            async fn invalid(&self) -> async_graphql::Result<&str> {
                Err(app_error_to_graphql(&AppError::Validation {
                    details: vec![ValidationErrorDetail {
                        field: "email".into(),
                        message: "must be a valid email".into(),
                    }],
                }))
            }
        }

        // Default config disables introspection.
        let default_app = graphql_routes_from_builder(
            Schema::build(ContextQuery, EmptyMutation, EmptySubscription),
            GraphQlConfig::default(),
        );
        let intro_res = default_app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/graphql")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"query":"{ __schema { types { name } } }"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = to_bytes(intro_res.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(
            json["data"]["__schema"].is_null()
                || json["errors"].as_array().is_some_and(|e| !e.is_empty()),
            "introspection must be disabled by default: {json}"
        );

        // Explicit with_introspection(true) enables introspection.
        let intro_enabled_app = graphql_routes_from_builder(
            Schema::build(ContextQuery, EmptyMutation, EmptySubscription),
            GraphQlConfig::default().with_introspection(true),
        );
        let intro_ok = intro_enabled_app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/graphql")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"query":"{ __schema { types { name } } }"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = to_bytes(intro_ok.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(json["data"]["__schema"]["types"].is_array());

        // Forward RequestId, TraceContext, AuthUser, Tenant from Axum Extensions into resolvers.
        let tc = TraceContext::generate();
        let mut req = Request::builder()
            .method("POST")
            .uri("/graphql")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"query":"{ whoami }"}"#))
            .unwrap();
        req.extensions_mut().insert(RequestId::from("req-42"));
        req.extensions_mut().insert(tc.clone());
        req.extensions_mut().insert(AuthUser::new("user-99"));
        req.extensions_mut().insert(Tenant::new("tenant-7", "acme"));

        let res = default_app.clone().oneshot(req).await.unwrap();
        let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            json["data"]["whoami"],
            format!("req-42|{}|user-99|tenant-7", tc.trace_id())
        );

        // Validation error details are preserved in GraphQL error extensions.
        let val_res = default_app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/graphql")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"query":"{ invalid }"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = to_bytes(val_res.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["errors"][0]["extensions"]["code"], "VALIDATION_FAILED");
        assert_eq!(json["errors"][0]["extensions"]["status"], 422);
        assert_eq!(
            json["errors"][0]["extensions"]["details"][0]["field"],
            "email"
        );
        assert_eq!(
            json["errors"][0]["extensions"]["details"][0]["message"],
            "must be a valid email"
        );
    }
}
