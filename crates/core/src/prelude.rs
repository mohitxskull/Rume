//! Convenience re-exports for standard Rume applications.

pub use crate::FromRow;
pub use crate::HasOpenApiSchema;
pub use crate::app::{
    AppContext, ApplicationMode, Availability, AvailabilityState, BuildError, Environment,
    IntoBindAddr, Lifecycle, LifecycleState, ProjectPaths, RequestId, Rume, RumeBuilder,
};
#[cfg(feature = "nats")]
pub use crate::broker::NatsBroker;
#[cfg(feature = "rabbitmq")]
pub use crate::broker::RabbitMqBroker;
pub use crate::broker::{InMemoryBroker, MessageBroker};
pub use crate::client::{RumeClient, RumeClientBuilder, ServiceDiscovery};
pub use crate::cron::{CronJobSpec, CronScheduler};
pub use crate::error::AppError;
pub use crate::events::{DistributedEventBus, Event, EventBus};
#[cfg(feature = "mysql")]
pub use crate::jobs::MysqlJobQueue;
#[cfg(feature = "postgres")]
pub use crate::jobs::PostgresJobQueue;
#[cfg(any(feature = "sqlite", feature = "postgres", feature = "mysql"))]
pub use crate::jobs::SqlBatchStore;
#[cfg(feature = "sqlite")]
pub use crate::jobs::SqliteJobQueue;
pub use crate::jobs::{JobBatch, JobQueue};
pub use crate::module::{Module, ModuleContext};
pub use crate::persistence::{
    CursorPage, DynamicSelect, OffsetPage, OptimisticLock, ReadWritePool, SoftDeletable,
};
#[cfg(feature = "postgres")]
pub use crate::pubsub::PostgresPubSub;
#[cfg(feature = "redis")]
pub use crate::pubsub::RedisPubSub;
pub use crate::pubsub::{InMemoryPubSub, PubSub};
pub use crate::routing::{
    HealthCheck, HealthChecks, HealthReport, HealthStatus, Matcher, RouteGroup, RouteInfo,
    RouteRegistry, RouteSpec, Router,
};
#[cfg(feature = "redis")]
pub use crate::saga::RedisSagaStore;
#[cfg(any(feature = "sqlite", feature = "postgres", feature = "mysql"))]
pub use crate::saga::SqlSagaStore;
pub use crate::saga::{InMemorySagaStore, Saga, SagaStore};
pub use crate::storage::ObjectStore;
#[cfg(feature = "testing")]
pub use crate::testing::{RumeTestingExt, TestApp, TestAppBuilder};
pub use crate::upload::ValidatedMultipart;
pub use crate::validation::{
    Validated, ValidatedForm, ValidatedJson, ValidatedPath, ValidatedQuery,
};
pub use axum::Json;
pub use axum::extract::Path;
pub use axum::extract::State;
pub use serde::{Deserialize, Serialize};
