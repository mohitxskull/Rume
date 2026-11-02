//! Routing for Rume applications.
//!
//! Provides an Axum-compatible routing layer with named routes, parameter
//! validation, route groups, resource routes, redirects, and URL building.
//!
//! # Example
//!
//! ```ignore
//! use rume::routing::{RouteSpec, Matcher};
//! use rume::prelude::*;
//!
//! let (path, method, _, _) = RouteSpec::get("/users/{id}")
//!     .where_param("id", Matcher::Number)
//!     .as("users.show")
//!     .build();
//!
//! let router = Router::new()
//!     .route(&path, match method {
//!         Method::GET => axum::routing::get(handlers::get_user),
//!         _ => axum::routing::any(handlers::get_user),
//!     });
//!
//! Rume::builder(ctx).router(router).run().await?;
//! ```

pub mod group;
pub mod health;
pub mod matchers;
pub mod registry;
pub mod rejection;
pub mod resource;
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
pub use spec::RouteSpec;
pub use validated_param::ValidatedParam;
