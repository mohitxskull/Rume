//! Job reliability traits: idempotency stores and worker heartbeat registries.

use std::fmt::Debug;
use std::time::Duration;

use crate::jobs::error::JobError;
use crate::jobs::job::BoxFuture;

/// Status returned when attempting to acquire an idempotency lock for a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdempotencyStatus {
    /// Key was not previously acquired; execution may proceed.
    New,
    /// Execution for this key is currently in progress by another worker.
    InProgress,
    /// Execution for this key previously succeeded; contains optional cached result bytes.
    Completed(Option<Vec<u8>>),
}

/// Storage interface for ensuring exactly-once or at-most-once job processing via idempotency keys.
pub trait IdempotencyStore: Debug + Send + Sync + 'static {
    /// Attempts to acquire an idempotency lock for `key` valid for `ttl`.
    fn try_acquire(
        &self,
        key: &str,
        ttl: Duration,
    ) -> BoxFuture<'_, Result<IdempotencyStatus, JobError>>;

    /// Marks the execution as completed, saving an optional result payload.
    fn complete(&self, key: &str, result: Option<Vec<u8>>) -> BoxFuture<'_, Result<(), JobError>>;

    /// Releases an in-progress lock (e.g. after failure), permitting re-execution.
    fn release(&self, key: &str) -> BoxFuture<'_, Result<(), JobError>>;

    /// Attempts to acquire an idempotency lock for `key` valid for `ttl` with owner token fencing.
    fn try_acquire_with_token(
        &self,
        key: &str,
        _token: &str,
        ttl: Duration,
    ) -> BoxFuture<'_, Result<IdempotencyStatus, JobError>> {
        self.try_acquire(key, ttl)
    }

    /// Marks the execution as completed with owner token validation, saving an optional result payload.
    fn complete_with_token(
        &self,
        key: &str,
        _token: &str,
        result: Option<Vec<u8>>,
    ) -> BoxFuture<'_, Result<(), JobError>> {
        self.complete(key, result)
    }

    /// Releases an in-progress lock with owner token validation, permitting re-execution.
    fn release_with_token(
        &self,
        key: &str,
        _token: &str,
    ) -> BoxFuture<'_, Result<(), JobError>> {
        self.release(key)
    }
}

/// Formats an idempotency key with length-prefixed tenant isolation to prevent collisions.
pub fn scoped_idempotency_key(tenant_id: &str, key: &str) -> String {
    format!("tenant:{}:{}:idemp:{key}", tenant_id.len(), tenant_id)
}

/// Registry tracking distributed worker heartbeats and detecting dead nodes.
pub trait HeartbeatRegistry: Debug + Send + Sync + 'static {
    /// Records or updates a heartbeat for a worker, valid for `ttl`.
    fn record_heartbeat(
        &self,
        worker_id: &str,
        ttl: Duration,
    ) -> BoxFuture<'_, Result<(), JobError>>;

    /// Checks if a worker's heartbeat is currently active.
    fn is_alive(&self, worker_id: &str) -> BoxFuture<'_, Result<bool, JobError>>;

    /// Removes and returns identifiers of workers whose heartbeat has expired.
    fn reap_dead_workers(&self) -> BoxFuture<'_, Result<Vec<String>, JobError>>;
}
