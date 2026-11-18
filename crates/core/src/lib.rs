//! Rume: a complete, modular Rust application framework built on Axum.
//!
//! Rume combines progressive complexity with strong compile-time conventions.
//! Small applications start with minimal boilerplate, while enterprise applications
//! leverage built-in modules, persistence, background jobs, multi-tenancy, transactional
//! outbox, sagas, resilience, authentication, and architectural verification.
//!
//! # Stability Tiers & SemVer Guarantees
//!
//! Rume categorizes its subsystem APIs into explicit stability tiers with concrete pre-1.0 contracts:
//!
//! - **Stable (Tier 1)**: Core HTTP shell, application lifecycle, typed configuration, RFC 9457 error contracts, and module composition (`app`, `config`, `env`, `error`, `module`, `routing`, `helpers`).
//!   *Contract:* Frozen API contracts. Breaking changes require major or exceptional minor transitions with explicit migration pathways.
//! - **Evolving (Tier 2)**: Persistence, caching, resilience, background jobs, transactional outbox, and security headers (`persistence`, `cache`, `resilience`, `jobs`, `outbox`, `security`, `auth`).
//!   *Contract:* Controlled evolution. Backwards-compatible across minor iterations with deprecation notices before API retirement.
//! - **Experimental (Tier 3)**: Enterprise SAML/SCIM, sagas, and distributed multi-tenant coordination (`enterprise`, `saga`, `coordination`, `tenant`).
//!   *Contract:* Active design iteration. May change across versions based on validation workloads without formal deprecation windows.

#![forbid(unsafe_code)]

pub use rume_app::app;
pub use rume_data::cache;
pub use rume_data::coordination;
pub use rume_enterprise::enterprise;
pub use rume_foundation::config;
pub use rume_foundation::env;
pub use rume_foundation::error;
pub use rume_foundation::helpers;
pub use rume_messaging::events;
pub use rume_messaging::jobs;
pub use rume_security::auth;
pub mod metrics;
pub mod module;
pub use rume_data::persistence;
pub use rume_http::openapi;
pub use rume_messaging::outbox;
pub mod prelude;
pub use rume_http::resilience;
pub mod routing;
pub use rume_enterprise::saga;
pub use rume_http::validation;
pub use rume_security::security;
pub use rume_security::tenant;
pub use rume_testing as testing;

pub use app::{
    AppContext, ApplicationMode, Availability, AvailabilityState, BuildError, Environment,
    Lifecycle, LifecycleState, ProjectPaths, RequestId, Rume, RumeBuilder, X_REQUEST_ID,
    init_tracing, try_init_tracing,
};
pub use error::AppError;
pub use events::{Event, EventBus};
pub use module::{Module, ModuleContext};
pub use routing::Router;
pub use rume_data::{MetricsRegistryPoolExt, params};
pub use rume_macros::{embed_migrations, sql_assets};
