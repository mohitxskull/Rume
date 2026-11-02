//! Rume: an explicit, typed, production-ready application shell for Axum.
//!
//! This crate is the core of the Rume framework. It is intentionally small:
//! optional features (auth, mail, jobs, storage, database helpers) live in
//! separate `rume-*` crates that depend on this one.

#![forbid(unsafe_code)]

pub mod app;
pub mod auth;
pub mod cache;
pub mod config;
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
pub mod tenant;
pub mod testing;
pub mod validation;

pub use app::{
    AppContext, ApplicationMode, Availability, AvailabilityState, Environment, Lifecycle,
    LifecycleState, MiddlewareConfigError, ObservabilityLayer, ProjectPaths, RequestId, Rume,
    RumeBuilder, X_REQUEST_ID, init_tracing, ops_router,
};
pub use auth::{
    ApiKeyScheme, AuthError, AuthLayer, AuthUser, Authenticated, BearerTokenScheme, CustomPolicy,
    Identity, MemorySessionStore, OptionalAuth, Policy, PolicyLayer, RequireAllPermissions,
    RequireAnyRole, RequirePermission, RequireRole, Session, SessionScheme, SessionStore,
    authorize,
};
pub use cache::{Cache, CacheBackend, CacheConfig, CacheError, MemoryCacheBackend};
pub use error::AppError;
pub use events::{Event, EventBus, EventError, EventHandler};
pub use helpers::{
    camel_case, condense_whitespace, dot_case, escape_html, excerpt, format_bytes, format_duration,
    kebab_case, mask_email, mask_secret, ordinal, parse_bytes, parse_duration, pascal_case,
    safe_equal, sentence_case, slug, snake_case, title_case, to_sentence, truncate,
};
pub use jobs::{
    HeartbeatRegistry, IdempotencyStatus, IdempotencyStore, Job, JobContext, JobError, JobHandler,
    JobQueue, JobRegistry, JobWorker, MemoryHeartbeatRegistry, MemoryIdempotencyStore,
    MemoryJobQueue, QueuedJob, RetryPolicy,
};
pub use metrics::{Counter, Gauge, MetricsLayer, MetricsRegistry};
pub use module::{
    ArchitectureRules, ArchitectureViolation, Module, ModuleContext, ModuleError, ModuleHarness,
};
pub use openapi::{
    OpenApiInfo, OpenApiOperation, OpenApiParameter, OpenApiParameterIn, OpenApiPathItem,
    OpenApiResponses, OpenApiSchema, OpenApiSpec, generate_openapi,
};
pub use outbox::{
    MemoryOutboxRepository, OutboxError, OutboxMessage, OutboxRelay, OutboxRepository, OutboxStatus,
};
pub use persistence::{
    DatabaseConfig, DatabasePool, MemoryDatabase, Migration, Migrator, PersistenceError,
};
pub use resilience::{
    CircuitBreaker, CircuitBreakerConfig, CircuitBreakerLayer, CircuitState, ConcurrencyLimitLayer,
    ConcurrencyLimiter, RateLimitConfig, RateLimitLayer, RateLimitStatus, RateLimiter,
    ResilienceError, TimeoutLayer,
};
pub use routing::{
    HealthCheck, HealthChecks, HealthReport, HealthStatus, Matcher, RouteGroup, RouteInfo,
    RouteRegistry, RouteSpec, Router, ValidatedParam,
};
pub use tenant::{
    HeaderTenantResolver, OptionalTenant, SubdomainTenantResolver, Tenant, TenantContext,
    TenantError, TenantLayer, TenantResolver, TenantScopedCache,
};
pub use testing::{TestApp, TestAppBuilder};
pub use validation::{Validated, ValidatedForm, ValidatedJson, ValidatedPath, ValidatedQuery};
