//! Rume: a complete, modular Rust application framework built on Axum.
//!
//! Rume combines progressive complexity with strong compile-time conventions.
//! Small applications start with minimal boilerplate, while enterprise applications
//! leverage built-in modules, persistence, background jobs, multi-tenancy, transactional
//! outbox, sagas, resilience, authentication, and architectural verification.
//!
//! # Stability Tiers
//!
//! Rume categorizes its subsystem APIs into explicit stability tiers:
//! - **Stable (Tier 1)**: Core HTTP shell, application lifecycle, typed configuration, RFC 9457 error contracts, and module composition (`app`, `config`, `env`, `error`, `module`, `routing`, `helpers`). Frozen pre-1.0 contracts.
//! - **Evolving (Tier 2)**: Persistence, caching, resilience, background jobs, transactional outbox, and security headers (`persistence`, `cache`, `resilience`, `jobs`, `outbox`, `security`, `auth`). Backwards-compatible within minor releases.
//! - **Experimental (Tier 3)**: Enterprise SAML/SCIM, sagas, and distributed multi-tenant coordination (`enterprise`, `saga`, `coordination`, `tenant`). Active evolution subject to refinement.

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
