//! Convenience re-exports for standard Rume applications.

pub use crate::app::{
    AppContext, ApplicationMode, Availability, AvailabilityState, BuildError, Environment,
    IntoBindAddr, Lifecycle, LifecycleState, ProjectPaths, RequestId, Rume, RumeBuilder,
};
pub use crate::error::AppError;
pub use crate::module::{Module, ModuleContext, ModuleHarness};
pub use crate::routing::{
    HealthCheck, HealthChecks, HealthReport, HealthStatus, Matcher, RouteGroup, RouteInfo,
    RouteRegistry, RouteSpec, Router, ValidatedParam,
};
pub use crate::testing::{TestApp, TestAppBuilder};
pub use crate::validation::{
    Validated, ValidatedForm, ValidatedJson, ValidatedPath, ValidatedQuery,
};
pub use axum::Json;
pub use axum::extract::Path;
pub use axum::extract::State;
pub use serde::{Deserialize, Serialize};
