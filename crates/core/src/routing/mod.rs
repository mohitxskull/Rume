//! Routing for Rume applications.
//!
//! Provides an Axum-compatible routing layer with named routes, parameter
//! validation, route groups, resource routes, redirects, and URL building.
//!
//! # Example
//!
//! ```
//! use rume::routing::{RouteSpec, Matcher, Router};
//!
//! let spec = RouteSpec::get("/users/{id}")
//!     .where_param("id", Matcher::Number)
//!     .r#as("users.show");
//!
//! let router = Router::<()>::new().route_spec(spec, || async { "ok" });
//! ```

pub mod group;
pub mod health;
pub mod matchers;
pub mod registry;
pub mod rejection;
pub mod resource;
pub mod router;
pub mod spec;
pub mod validated_param;

// Re-export the public routing surface at the module root for convenience.
pub use group::RouteGroup;
pub use health::{
    HealthCheck, HealthCheckResult, HealthChecks, HealthReport, HealthStatus, health_routes,
};
pub use matchers::Matcher;
pub use registry::{RouteInfo, RouteRegistry};
pub use rejection::Rejection;
pub use resource::Resource;
pub use router::Router;
pub use spec::{ParamValidationLayer, RouteSpec};
pub use validated_param::ValidatedParam;
