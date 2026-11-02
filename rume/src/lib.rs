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
pub mod module;
pub mod prelude;
pub mod routing;
pub mod testing;
pub mod validation;

pub use app::{
    AppContext, ApplicationMode, Availability, AvailabilityState, Environment, Lifecycle,
    LifecycleState, MiddlewareConfigError, ObservabilityLayer, ProjectPaths, RequestId, Rume,
    RumeBuilder, X_REQUEST_ID, init_tracing, ops_router,
};
pub use error::AppError;
pub use helpers::{
    camel_case, condense_whitespace, dot_case, escape_html, excerpt, format_bytes, format_duration,
    kebab_case, mask_email, mask_secret, ordinal, parse_bytes, parse_duration, pascal_case,
    safe_equal, sentence_case, slug, snake_case, title_case, to_sentence, truncate,
};
pub use module::{Module, ModuleContext, ModuleError};
pub use routing::{
    HealthCheck, HealthChecks, HealthReport, HealthStatus, Matcher, RouteGroup, RouteInfo,
    RouteRegistry, RouteSpec, Router, ValidatedParam,
};
pub use testing::{TestApp, TestAppBuilder};
pub use validation::{Validated, ValidatedForm, ValidatedJson, ValidatedPath, ValidatedQuery};
