//! Rume: a complete, modular Rust application framework built on Axum.
//!
//! Rume combines progressive complexity with strong compile-time conventions.
//! Small applications start with minimal boilerplate, while enterprise applications
//! leverage built-in modules, persistence, background jobs, multi-tenancy, transactional
//! outbox, sagas, resilience, authentication, and architectural verification.

#![forbid(unsafe_code)]

pub mod app;
pub mod auth;
pub mod cache;
pub mod config;
pub mod coordination;
pub mod enterprise;
pub mod env;
pub mod error;
pub mod events;
pub mod helpers;
pub mod jobs;
pub mod metrics;
pub mod module;
pub mod openapi;
pub mod outbox;
pub mod persistence;
pub mod prelude;
pub mod resilience;
pub mod routing;
pub mod saga;
pub mod security;
pub mod tenant;
pub mod testing;
pub mod validation;

pub use app::{
    AppContext, ApplicationMode, Availability, AvailabilityState, BuildError, Environment,
    Lifecycle, LifecycleState, ProjectPaths, RequestId, Rume, RumeBuilder, X_REQUEST_ID,
    init_tracing, try_init_tracing,
};
pub use error::AppError;
pub use events::{Event, EventBus};
pub use module::{Module, ModuleContext};
pub use routing::Router;
