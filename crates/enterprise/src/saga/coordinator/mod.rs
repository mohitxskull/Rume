//! Saga workflow coordinator, runner, and persistence store.

use std::future::Future;
use std::pin::Pin;

#[cfg(feature = "redis")]
mod redis_store;
#[cfg(any(feature = "sqlite", feature = "postgres", feature = "mysql"))]
mod sql_store;
mod store;
mod workflow;

#[cfg(feature = "redis")]
pub use redis_store::RedisSagaStore;
#[cfg(any(feature = "sqlite", feature = "postgres", feature = "mysql"))]
pub use sql_store::SqlSagaStore;
pub use store::{InMemorySagaStore, SagaRecord, SagaStatus, SagaStore};
pub use workflow::{
    Saga, SagaRecoveryError, SagaRecoveryHandle, SagaRecoveryPhase, SagaReport,
    compensation_idempotency_key, forward_idempotency_key,
};

/// Pinned boxed future for saga store operations.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[cfg(test)]
mod tests;
