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
//! - **Stable (Tier 1)**: Core HTTP shell, application lifecycle, typed configuration, RFC 9457 error contracts, and module composition (`app`, `config`, `env`, `error`, `module`, `routing`, `util`).
//!   *Contract:* Frozen API contracts. Breaking changes require major or exceptional minor transitions with explicit migration pathways.
//! - **Evolving (Tier 2)**: Persistence, caching, resilience, background jobs, transactional outbox, and security headers (`persistence`, `cache`, `resilience`, `jobs`, `outbox`, `security`, `auth`).
//!   *Contract:* Controlled evolution. Backwards-compatible across minor iterations with deprecation notices before API retirement.
//! - **Experimental (Tier 3)**: Enterprise SAML/SCIM, sagas, and distributed multi-tenant coordination (`enterprise`, `saga`, `coordination`, `tenant`).
//!   *Contract:* Active design iteration. May change across versions based on validation workloads without formal deprecation windows.

#![forbid(unsafe_code)]

pub use rume_app::app;
pub use rume_data::cache;
pub use rume_data::coordination;
pub use rume_data::persistence;
pub use rume_data::sql;
pub use rume_data::storage;
pub use rume_enterprise::enterprise;
pub use rume_enterprise::saga;
pub use rume_foundation::config;
pub use rume_foundation::env;
pub use rume_foundation::error;
pub use rume_foundation::util;
#[cfg(feature = "graphql")]
pub use rume_graphql as graphql;
#[cfg(feature = "grpc")]
pub use rume_grpc as grpc;
pub use rume_http::client;
pub use rume_http::http_cache;
pub use rume_http::pagination;
pub use rume_http::realtime;
pub use rume_http::resilience;
#[cfg(feature = "scim")]
pub use rume_http::scim;
pub use rume_http::static_files;
#[cfg(feature = "tls")]
pub use rume_http::tls;
pub use rume_http::transport;
pub use rume_http::upload;
pub use rume_http::validation;
pub use rume_messaging::broker;
pub use rume_messaging::cron;
pub use rume_messaging::events;
pub use rume_messaging::jobs;
pub use rume_messaging::mailer;
pub use rume_messaging::outbox;
pub use rume_messaging::pubsub;
pub use rume_observability::metrics;
pub use rume_observability::prometheus;
pub use rume_observability::redaction;
pub use rume_security::auth;
pub use rume_security::flags;
pub use rume_security::metering;
pub use rume_security::security;
pub use rume_security::tenant;
#[cfg(feature = "testing")]
pub use rume_testing as testing;

pub mod prelude;

/// OpenAPI v3.1 specification, docs UI, and `HasOpenApiSchema` derive.
pub mod openapi {
    pub use rume_http::openapi::*;
    pub use rume_macros::HasOpenApiSchema;
}

/// First-class application modules (re-exported from `rume-app` and `rume-testing`).
pub mod module {
    pub use rume_app::module::*;
    #[cfg(feature = "testing")]
    pub use rume_testing::{ModuleHarness, ModuleTestHarness};
}

/// Outbound webhook dispatching/signing and inbound HMAC-SHA256 verification.
pub mod webhooks {
    pub use rume_http::webhooks::*;
    pub use rume_messaging::webhooks::*;
}

/// Routing re-exports from `rume-http` with `AppContext`-typed `health_routes`.
pub mod routing {
    pub use rume_http::routing::*;

    /// Routes for health checks (`/health/live` and `/health/ready`) bound to [`crate::app::AppContext`].
    pub fn health_routes<E, C, S>() -> Router<crate::app::AppContext<E, C, S>>
    where
        E: Clone + Send + Sync + 'static,
        C: Clone + Send + Sync + 'static,
        S: Clone + Send + Sync + 'static,
    {
        rume_http::routing::health_routes::<crate::app::AppContext<E, C, S>>()
    }
}

pub use app::{
    AppContext, ApplicationMode, Availability, AvailabilityState, BuildError, Environment,
    Lifecycle, LifecycleState, ProjectPaths, RequestId, Rume, RumeBuilder, TaskHandler, TaskInfo,
    TaskRegistry, X_REQUEST_ID, init_tracing, shutdown_global_observability,
    shutdown_global_observability_async, try_init_tracing,
};
pub use error::AppError;
pub use events::{DistributedEventBus, Event, EventBus};
pub use module::{Module, ModuleContext};
pub use routing::Router;
pub use rume_data::{MetricsRegistryPoolExt, params};
pub use rume_macros::{FromRow, HasOpenApiSchema, embed_migrations, sql_assets};
