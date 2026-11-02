//! Rume: an explicit, typed, production-ready application shell for Axum.
//!
//! This crate is the core of the Rume framework. It is intentionally small:
//! optional features (auth, mail, jobs, storage, database helpers) live in
//! separate `rume-*` crates that depend on this one.

#![forbid(unsafe_code)]

pub mod app;
pub mod config;
pub mod env;
pub mod error;
pub mod helpers;
pub mod prelude;
pub mod routing;
pub mod testing;
pub mod validation;

pub use app::{
    AppContext, ApplicationMode, Availability, AvailabilityState, Environment, Lifecycle,
    LifecycleState, ProjectPaths, Rume, RumeBuilder, ops_router,
};
pub use error::AppError;
pub use helpers::{camel_case, kebab_case, pascal_case, safe_equal, slug, snake_case};
pub use routing::{
    HealthCheck, HealthChecks, HealthReport, HealthStatus, Matcher, RouteGroup, RouteInfo,
    RouteRegistry, RouteSpec, ValidatedParam,
};
pub use testing::{TestApp, TestAppBuilder};
pub use validation::Validated;
