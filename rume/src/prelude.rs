//! Convenience re-exports for Rume applications.

pub use crate::app::{
    AppContext, ApplicationMode, Availability, AvailabilityState, Environment, Lifecycle,
    LifecycleState, MiddlewareConfigError, ProjectPaths, Rume, RumeBuilder, init_tracing,
    ops_router,
};
pub use crate::config;
pub use crate::config::ConfigError;
pub use crate::env;
pub use crate::env::EnvError;
pub use crate::error::AppError;
pub use crate::helpers::{safe_equal, slug};
pub use crate::routing::{
    HealthCheck, HealthChecks, HealthReport, HealthStatus, Matcher, RouteGroup, RouteInfo,
    RouteRegistry, RouteSpec, ValidatedParam,
};
pub use crate::testing::{TestApp, TestAppBuilder};
pub use crate::validation::Validated;
pub use axum::extract::Path;
pub use axum::extract::State;
pub use axum::{Json, Router};
pub use serde::{Deserialize, Serialize};
