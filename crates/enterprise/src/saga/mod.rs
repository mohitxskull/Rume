//! Workflow orchestration using the Saga pattern with reversible compensation.

pub mod coordinator;
pub mod error;
pub mod step;

pub use coordinator::{
    InMemorySagaStore, Saga, SagaRecord, SagaRecoveryError, SagaRecoveryHandle, SagaRecoveryPhase,
    SagaReport, SagaStatus, SagaStore, compensation_idempotency_key, forward_idempotency_key,
};
pub use error::SagaError;
pub use step::{FunctionalStep, SagaFuture, Step};
