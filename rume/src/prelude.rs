//! Convenience re-exports for Rume applications.

pub use crate::app::{
    AppContext, ApplicationMode, Availability, AvailabilityState, BuildError, Environment,
    IntoBindAddr, Lifecycle, LifecycleState, MiddlewareConfigError, ProjectPaths, RequestId, Rume,
    RumeBuilder, init_tracing, ops_router,
};
pub use crate::auth::{
    ApiKeyScheme, AuthError, AuthLayer, AuthUser, Authenticated, BearerTokenScheme, CustomPolicy,
    Identity, MemorySessionStore, OptionalAuth, Policy, PolicyLayer, RequireAllPermissions,
    RequireAnyRole, RequirePermission, RequireRole, Session, SessionScheme, SessionStore,
    authorize,
};
pub use crate::cache::{Cache, CacheBackend, CacheConfig, CacheError, MemoryCacheBackend};
pub use crate::config;
pub use crate::config::ConfigError;
pub use crate::env;
pub use crate::env::EnvError;
pub use crate::error::AppError;
pub use crate::events::{Event, EventBus, EventError, EventHandler};
pub use crate::helpers::{
    condense_whitespace, escape_html, format_bytes, format_duration, mask_email, mask_secret,
    safe_equal, slug, truncate,
};
pub use crate::jobs::{
    HeartbeatRegistry, IdempotencyStatus, IdempotencyStore, Job, JobContext, JobError, JobHandler,
    JobQueue, JobRegistry, JobWorker, MemoryHeartbeatRegistry, MemoryIdempotencyStore,
    MemoryJobQueue, QueuedJob, RetryPolicy,
};
pub use crate::metrics::{Counter, Gauge, MetricsLayer, MetricsRegistry};
pub use crate::module::{
    ArchitectureRules, ArchitectureViolation, Module, ModuleContext, ModuleError, ModuleHarness,
};
pub use crate::openapi::{
    OpenApiInfo, OpenApiOperation, OpenApiParameter, OpenApiParameterIn, OpenApiPathItem,
    OpenApiResponses, OpenApiSchema, OpenApiSpec, generate_openapi,
};
pub use crate::outbox::{
    MemoryOutboxRepository, OutboxError, OutboxMessage, OutboxRelay, OutboxRepository, OutboxStatus,
};
pub use crate::persistence::{
    DatabaseConfig, DatabasePool, MemoryDatabase, Migration, Migrator, PersistenceError,
};
pub use crate::resilience::{
    CircuitBreaker, CircuitBreakerConfig, CircuitBreakerLayer, CircuitState, ConcurrencyLimitLayer,
    ConcurrencyLimiter, RateLimitConfig, RateLimitLayer, RateLimitStatus, RateLimiter,
    ResilienceError, TimeoutLayer,
};
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
