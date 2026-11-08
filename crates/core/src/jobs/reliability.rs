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
    /// Attempts to acquire an idempotency lock for `key` valid for `ttl` with owner token fencing.
    fn try_acquire_with_token(
        &self,
        key: &str,
        token: &str,
        ttl: Duration,
    ) -> BoxFuture<'_, Result<IdempotencyStatus, JobError>>;

    /// Marks the execution as completed with owner token validation, saving an optional result payload.
    fn complete_with_token(
        &self,
        key: &str,
        token: &str,
        result: Option<Vec<u8>>,
    ) -> BoxFuture<'_, Result<(), JobError>>;

    /// Releases an in-progress lock with owner token validation, permitting re-execution.
    fn release_with_token(&self, key: &str, token: &str) -> BoxFuture<'_, Result<(), JobError>>;

    /// Attempts to acquire an idempotency lock for `key` valid for `ttl` without owner token fencing.
    ///
    /// Delegates to [`IdempotencyStore::try_acquire_with_token`] with an empty token.
    fn try_acquire(
        &self,
        key: &str,
        ttl: Duration,
    ) -> BoxFuture<'_, Result<IdempotencyStatus, JobError>> {
        self.try_acquire_with_token(key, "", ttl)
    }

    /// Marks the execution as completed, saving an optional result payload.
    ///
    /// Delegates to [`IdempotencyStore::complete_with_token`] with an empty token.
    fn complete(&self, key: &str, result: Option<Vec<u8>>) -> BoxFuture<'_, Result<(), JobError>> {
        self.complete_with_token(key, "", result)
    }

    /// Releases an in-progress lock (e.g. after failure), permitting re-execution.
    ///
    /// Delegates to [`IdempotencyStore::release_with_token`] with an empty token.
    fn release(&self, key: &str) -> BoxFuture<'_, Result<(), JobError>> {
        self.release_with_token(key, "")
    }

    /// Extends the lease/lock expiration for an in-progress idempotency key if the caller holds the matching token.
    ///
    /// # Safety and Distributed Invariants (CRITICAL — DO NOT DEFAULT TO `Ok(())`)
    /// This method **MUST fail closed** by returning [`JobError::Unsupported`] when unimplemented.
    ///
    /// ## Why a silent `Ok(())` default is dangerous:
    /// In an asynchronous job processing system, long-running jobs depend on heartbeat renewal
    /// of their idempotency lock. If an idempotency store silently returns `Ok(())` without actually
    /// extending the TTL in the backend (e.g. Redis `PEXPIRE`), the lock on the remote store will
    /// expire while the worker is still executing. A second worker or retrying queue will then
    /// acquire the same idempotency key and execute the job concurrently, causing duplicate external
    /// side effects (double billing, duplicate emails, etc.).
    ///
    /// By returning `Err(JobError::Unsupported)`, custom stores that do not implement heartbeat
    /// lock extension cause the worker's heartbeat loop to immediately detect that renewal is not
    /// supported, fail closed, mark the lease as lost, and route the job to DLQ rather than
    /// falsely assuming idempotency protection.
    fn extend_lock_with_token(
        &self,
        key: &str,
        token: &str,
        extension: Duration,
    ) -> BoxFuture<'_, Result<(), JobError>> {
        let _ = (key, token, extension);
        Box::pin(async move {
            Err(JobError::Unsupported(
                "idempotency lock extension is not supported by this store".into(),
            ))
        })
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
