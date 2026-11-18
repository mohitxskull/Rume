//! Foundation types, configuration, environment loading, errors, and health contracts for Rume.

#![forbid(unsafe_code)]

pub mod config;
pub mod env;
pub mod error;
pub mod health;
pub mod helpers;
pub mod paths;
pub mod state;
pub mod telemetry;

pub use error::{AppError, ValidationErrorDetail};
pub use health::{
    HealthCheck, HealthCheckObserver, HealthCheckResult, HealthChecks, HealthReport, HealthStatus,
};
pub use paths::ProjectPaths;
pub use state::{
    ApplicationMode, Availability, AvailabilityState, Environment, Lifecycle, LifecycleState,
};
pub use telemetry::{PoolStatus, RequestId, X_REQUEST_ID};
