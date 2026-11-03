//! Outbox storage repository and transactional outbox contracts.

use std::fmt::Debug;
use std::future::Future;
use std::pin::Pin;

use crate::outbox::error::OutboxError;
use crate::outbox::message::OutboxMessage;
use crate::persistence::backend::DatabaseTransaction;

/// Pinned boxed future type alias for async outbox repository operations.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Storage contract for saving, querying, and updating outbox messages.
pub trait OutboxRepository: Debug + Send + Sync + 'static {
    /// Persists a new outbox message.
    fn save(&self, msg: OutboxMessage) -> BoxFuture<'_, Result<(), OutboxError>>;

    /// Fetches up to `limit` pending outbox messages ready for publication.
    fn fetch_pending(&self, limit: usize)
    -> BoxFuture<'_, Result<Vec<OutboxMessage>, OutboxError>>;

    /// Atomically claims up to `limit` pending outbox messages, transitioning their status
    /// to `InFlight` and assigning a lease token and default 30-second expiration timestamp.
    fn claim_pending(
        &self,
        limit: usize,
    ) -> BoxFuture<'_, Result<Vec<OutboxMessage>, OutboxError>> {
        self.claim_pending_with_lease(limit, 30)
    }

    /// Atomically claims up to `limit` pending outbox messages with a custom lease duration in seconds.
    fn claim_pending_with_lease(
        &self,
        limit: usize,
        lease_duration_secs: u64,
    ) -> BoxFuture<'_, Result<Vec<OutboxMessage>, OutboxError>>;

    /// Marks a message as successfully published with the worker's active lease token.
    fn mark_published(&self, id: &str, lease_token: &str) -> BoxFuture<'_, Result<(), OutboxError>>;

    /// Marks a message as failed with an error message and increments its retry counter,
    /// fenced by the worker's active lease token.
    fn mark_failed(
        &self,
        id: &str,
        lease_token: &str,
        error: String,
    ) -> BoxFuture<'_, Result<(), OutboxError>>;

    /// Recovers abandoned or timed-out in-flight messages whose claim lease has expired,
    /// resetting their status back to `Pending`.
    fn recover_expired_claims(
        &self,
        max_lease_age_secs: u64,
    ) -> BoxFuture<'_, Result<usize, OutboxError>> {
        let _ = max_lease_age_secs;
        Box::pin(async move { Ok(0) })
    }

    /// Returns the number of currently pending outbox messages.
    fn pending_count(&self) -> BoxFuture<'_, Result<usize, OutboxError>>;

    /// Returns the number of successfully published messages.
    fn published_count(&self) -> BoxFuture<'_, Result<usize, OutboxError>>;

    /// Returns the number of permanently failed (dead-lettered) messages.
    fn failed_count(&self) -> BoxFuture<'_, Result<usize, OutboxError>> {
        Box::pin(async move { Ok(0) })
    }

    /// Fetches up to `limit` permanently failed (dead-lettered) messages for inspection.
    fn fetch_failed(
        &self,
        _limit: usize,
    ) -> BoxFuture<'_, Result<Vec<OutboxMessage>, OutboxError>> {
        Box::pin(async move { Ok(vec![]) })
    }

    /// Resets a permanently failed message back to `Pending` status with reset retry count.
    fn reprocess_failed(&self, _id: &str) -> BoxFuture<'_, Result<(), OutboxError>> {
        Box::pin(async move { Ok(()) })
    }
}

/// Extension trait for outbox repositories supporting atomic persistence enlisted inside database transactions.
pub trait TransactionalOutbox<Tx: DatabaseTransaction>: OutboxRepository {
    /// Saves an outbox message inside an active database transaction.
    ///
    /// The message will only be committed if `tx.commit()` succeeds.
    /// If `tx.rollback()` is executed, the message is completely discarded.
    fn save_tx<'a>(
        &'a self,
        tx: &'a Tx,
        message: OutboxMessage,
    ) -> BoxFuture<'a, Result<(), OutboxError>>;
}
