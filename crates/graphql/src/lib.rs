//! Optional async-graphql integration with W3C TraceContext and `AppError`
//! extensions for Rume.

#![forbid(unsafe_code)]

pub mod error;
pub mod router;
pub mod trace;

pub use error::app_error_to_graphql;
pub use router::{GraphQlConfig, graphql_routes};
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

    #[Object]
    impl QueryRoot {
        async fn hello(&self) -> &str {
            "world"
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
}
