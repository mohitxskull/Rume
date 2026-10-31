//! Convenience re-exports for Rume applications.

pub use crate::app::{
    ApplicationMode, Availability, AvailabilityState, Environment, Lifecycle, LifecycleState,
    ProjectPaths, Rume, RumeBuilder, detected_ai_agent, managed_by_pm2, running_in_ai_agent,
};
pub use crate::config;
pub use crate::config::ConfigError;
pub use crate::env;
pub use crate::env::EnvError;
pub use crate::error::AppError;
pub use crate::validation::Validated;

pub use axum::routing;
pub use axum::{Json, Router};
pub use serde;
pub use serde::{Deserialize, Serialize};
pub use tokio;
pub use garde;
