//! HTTP routing, OpenAPI generation, request validation, and resilience middleware for Rume.

#![forbid(unsafe_code)]

pub mod client;
pub mod openapi;
pub mod pagination;
pub mod resilience;
pub mod routing;
pub mod transport;
pub mod upload;
pub mod validation;

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use rume_foundation::health::HealthChecks;
    use rume_foundation::state::{Availability, AvailabilityState};
    use tower::ServiceExt;

    use crate::openapi::{OpenApiInfo, OpenApiSchema, generate_openapi};
    use crate::routing::{HealthRoutesState, Matcher, RouteRegistry, RouteSpec, health_routes};

    #[test]
    fn http_openapi_generates_spec_from_route_registry_without_routing_cycle() {
        let mut registry = RouteRegistry::new();
        registry.register(
            RouteSpec::post("/users/{id}")
                .where_param("id", Matcher::Number)
                .r#as("users.update")
                .with_request_schema(OpenApiSchema::object())
                .into_route_info(),
        );

        let spec = generate_openapi(&registry, OpenApiInfo::new("Test API", "1.0.0"));
        assert_eq!(spec.openapi, "3.1.0");
        let path_item = spec.paths.get("/users/{id}").expect("missing route path");
        let post = path_item.post.as_ref().expect("missing POST operation");
        assert_eq!(post.operation_id.as_deref(), Some("users.update"));
        assert!(post.request_body.is_some());
        assert_eq!(post.parameters[0].schema.r#type, "integer");
    }

    #[tokio::test]
    async fn http_health_routes_return_503_when_refusing_traffic() {
        let availability = Availability::new();
        availability.set(AvailabilityState::RefusingTraffic);
        let state = HealthRoutesState {
            availability,
            health_checks: HealthChecks::new(),
        };

        let router = health_routes::<HealthRoutesState>().with_state(state);
        let response = router
            .oneshot(
                Request::builder()
                    .uri("/health/ready")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .expect("request failed");
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
