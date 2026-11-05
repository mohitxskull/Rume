//! Fast zero-docker integration suites.

// Why: #![deny(unsafe_code)] instead of #![forbid(unsafe_code)] is required to allow integration::config to use unsafe for process environment mutation via EnvVarGuard.
#![deny(unsafe_code)]

#[path = "common/mod.rs"]
pub mod common;

#[path = "integration/architecture.rs"]
pub mod architecture;

#[path = "integration/auth.rs"]
pub mod auth;

#[path = "integration/cache.rs"]
pub mod cache;

#[path = "integration/composition.rs"]
pub mod composition;

#[path = "integration/concurrency_invariants.rs"]
#[cfg(feature = "sqlite")]
pub mod concurrency_invariants;

#[path = "integration/config.rs"]
pub mod config;

#[path = "integration/enterprise.rs"]
pub mod enterprise;

#[path = "integration/error.rs"]
pub mod error;

#[path = "integration/error_redaction.rs"]
pub mod error_redaction;

#[path = "integration/events.rs"]
pub mod events;

#[path = "integration/health.rs"]
pub mod health;

#[path = "integration/http_smoke.rs"]
pub mod http_smoke;

#[path = "integration/lifecycle.rs"]
pub mod lifecycle;

#[path = "integration/metrics.rs"]
pub mod metrics;

#[path = "integration/openapi.rs"]
pub mod openapi;

#[path = "integration/property_tests.rs"]
pub mod property_tests;

#[path = "integration/outbox.rs"]
#[cfg(feature = "sqlite")]
pub mod outbox;

#[path = "integration/persistence.rs"]
#[cfg(feature = "sqlite")]
pub mod persistence;

#[path = "integration/request_identity.rs"]
pub mod request_identity;

#[path = "integration/resilience.rs"]
pub mod resilience;

#[path = "integration/route_metadata.rs"]
pub mod route_metadata;

#[path = "integration/routing.rs"]
pub mod routing;

#[path = "integration/saga.rs"]
pub mod saga;

#[path = "integration/security.rs"]
#[cfg(feature = "sqlite")]
pub mod security;

#[path = "integration/sqlite.rs"]
#[cfg(feature = "sqlite")]
pub mod sqlite;

#[path = "integration/tenant.rs"]
pub mod tenant;

#[path = "integration/test_server.rs"]
pub mod test_server;

#[path = "integration/validation.rs"]
pub mod validation;
