//! Convenience re-exports for standard Rume applications.

pub use crate::FromRow;
pub use crate::HasOpenApiSchema;
pub use crate::app::{
    AppContext, ApplicationMode, Availability, AvailabilityState, BuildError, Environment,
    IntoBindAddr, Lifecycle, LifecycleState, ProjectPaths, RequestId, Rume, RumeBuilder,
    TaskHandler, TaskInfo, TaskRegistry,
};
#[cfg(feature = "nats")]
pub use crate::broker::NatsBroker;
#[cfg(feature = "rabbitmq")]
pub use crate::broker::RabbitMqBroker;
pub use crate::broker::{InMemoryBroker, MessageBroker};
pub use crate::client::{RumeClient, RumeClientBuilder, ServiceDiscovery};
pub use crate::cron::{CronJobSpec, CronScheduler};
#[cfg(feature = "scim")]
pub use crate::enterprise::scim::{InMemoryScimStore, ScimStore};
pub use crate::env::{CachedSecretProvider, EnvSecretProvider, SecretProvider};
pub use crate::error::AppError;
pub use crate::events::{DistributedEventBus, Event, EventBus};
#[cfg(any(feature = "sqlite", feature = "postgres", feature = "mysql"))]
pub use crate::flags::SqlFeatureFlags;
pub use crate::flags::{EvaluationContext, FeatureFlag, FeatureFlagStore, InMemoryFeatureFlags};
pub use crate::http_cache::{HttpCacheConfig, HttpCacheLayer};
#[cfg(feature = "mysql")]
pub use crate::jobs::MysqlJobQueue;
#[cfg(feature = "postgres")]
pub use crate::jobs::PostgresJobQueue;
#[cfg(any(feature = "sqlite", feature = "postgres", feature = "mysql"))]
pub use crate::jobs::SqlBatchStore;
#[cfg(feature = "sqlite")]
pub use crate::jobs::SqliteJobQueue;
pub use crate::jobs::{JobBatch, JobQueue};
pub use crate::mailer::{EmailMessage, Mailer};
#[cfg(feature = "mailer")]
pub use crate::mailer::{SmtpConfig, SmtpMailer};
#[cfg(any(feature = "sqlite", feature = "postgres", feature = "mysql"))]
pub use crate::metering::SqlUsageMeter;
pub use crate::metering::{BillingSink, InMemoryUsageMeter, UsageMeter};
pub use crate::module::{Module, ModuleContext};
pub use crate::persistence::{
    CursorPage, DynamicSelect, OffsetPage, OptimisticLock, ReadWritePool, SoftDeletable,
};
#[cfg(feature = "postgres")]
pub use crate::pubsub::PostgresPubSub;
#[cfg(feature = "redis")]
pub use crate::pubsub::RedisPubSub;
pub use crate::pubsub::{InMemoryPubSub, PubSub};
pub use crate::realtime::{RealtimeAuth, RealtimeConfig, RealtimeEvent, RealtimeHub};
pub use crate::routing::{
    ApiVersion, ApiVersionExtractor, DeprecationLayer, HealthCheck, HealthChecks, HealthReport,
    HealthStatus, Matcher, RouteGroup, RouteInfo, RouteRegistry, RouteSpec, Router,
    VersionHeaderLayer,
};
#[cfg(feature = "redis")]
pub use crate::saga::RedisSagaStore;
#[cfg(any(feature = "sqlite", feature = "postgres", feature = "mysql"))]
pub use crate::saga::SqlSagaStore;
pub use crate::saga::{InMemorySagaStore, Saga, SagaStore};
#[cfg(feature = "scim")]
pub use crate::scim::scim_routes;
pub use crate::static_files::{StaticFilesConfig, static_files_router};
pub use crate::storage::ObjectStore;
#[cfg(feature = "testing")]
pub use crate::testing::TestMailer;
#[cfg(feature = "testing")]
pub use crate::testing::{RumeTestingExt, TestApp, TestAppBuilder};
#[cfg(feature = "acme")]
pub use crate::tls::AcmeConfig;
#[cfg(feature = "tls")]
pub use crate::tls::TlsConfig;
pub use crate::upload::ValidatedMultipart;
pub use crate::validation::{
    Validated, ValidatedForm, ValidatedJson, ValidatedPath, ValidatedQuery,
};
pub use crate::webhooks::{
    VerifyWebhookSignatureLayer, WebhookDispatcher, WebhookEndpoint, WebhookSignatureConfig,
};
pub use axum::Json;
pub use axum::extract::Path;
pub use axum::extract::State;
pub use serde::{Deserialize, Serialize};
