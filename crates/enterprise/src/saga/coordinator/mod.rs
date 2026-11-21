//! Saga workflow coordinator, runner, and persistence store.

use std::future::Future;
use std::pin::Pin;

mod store;
mod workflow;

pub use store::{InMemorySagaStore, SagaRecord, SagaStatus, SagaStore};
pub use workflow::{
    Saga, SagaRecoveryError, SagaRecoveryHandle, SagaRecoveryPhase, SagaReport,
    compensation_idempotency_key, forward_idempotency_key,
};

/// Pinned boxed future for saga store operations.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[cfg(test)]
mod tests;
