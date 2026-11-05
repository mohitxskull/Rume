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

    /// Extends the lease of an in-flight outbox message, preventing it from being
    /// recovered while dispatch is still in progress.
    ///
    /// Only updates the message if the token matches and the lease has not yet expired.
    /// The default implementation returns `Err(OutboxError::Unsupported)` to prevent
    /// repositories from silently appearing renewal-capable when they do not implement it.
    fn renew_outbox_lease(
        &self,
        id: &str,
        _lease_token: &str,
        _extension_secs: u64,
    ) -> BoxFuture<'_, Result<(), OutboxError>> {
        let msg_id = id.to_string();
        Box::pin(async move {
            Err(OutboxError::Unsupported(format!(
                "outbox lease renewal not supported by this repository backend (id={msg_id})"
            )))
        })
    }

    /// Marks a message as successfully published with the worker's active lease token.
    fn mark_published(&self, id: &str, lease_token: &str)
    -> BoxFuture<'_, Result<(), OutboxError>>;

    /// Marks a message as failed with an error message and increments its retry counter,
    /// fenced by the worker's active lease token.
    fn mark_failed(
        &self,
        id: &str,
        lease_token: &str,
        error: String,
    ) -> BoxFuture<'_, Result<(), OutboxError>> {
        self.mark_failed_with_max_retries(id, lease_token, error, 5)
    }

    /// Marks a message as failed with a configurable maximum retry limit before marking as failed.
    fn mark_failed_with_max_retries(
        &self,
        id: &str,
        lease_token: &str,
        error: String,
        _max_retries: u32,
    ) -> BoxFuture<'_, Result<(), OutboxError>> {
        self.mark_failed(id, lease_token, error)
    }

    /// Recovers abandoned or timed-out in-flight messages whose claim lease has expired,
    /// resetting their status back to `Pending`.
    fn recover_expired_claims(
        &self,
        max_lease_age_secs: u64,
    ) -> BoxFuture<'_, Result<usize, OutboxError>> {
        let _ = max_lease_age_secs;
        Box::pin(async move {
            Err(OutboxError::Unsupported(
                "recover_expired_claims not supported by this repository backend".into(),
            ))
        })
    }

    /// Returns the number of currently pending outbox messages.
    fn pending_count(&self) -> BoxFuture<'_, Result<usize, OutboxError>>;

    /// Returns the number of successfully published messages.
    fn published_count(&self) -> BoxFuture<'_, Result<usize, OutboxError>>;

    /// Returns the number of permanently failed (dead-lettered) messages.
    fn failed_count(&self) -> BoxFuture<'_, Result<usize, OutboxError>> {
        Box::pin(async move {
            Err(OutboxError::Unsupported(
                "failed_count not supported by this repository backend".into(),
            ))
        })
    }

    /// Fetches up to `limit` permanently failed (dead-lettered) messages for inspection.
    fn fetch_failed(
        &self,
        _limit: usize,
    ) -> BoxFuture<'_, Result<Vec<OutboxMessage>, OutboxError>> {
        Box::pin(async move {
            Err(OutboxError::Unsupported(
                "fetch_failed not supported by this repository backend".into(),
            ))
        })
    }

    /// Resets a permanently failed message back to `Pending` status with reset retry count.
    fn reprocess_failed(&self, id: &str) -> BoxFuture<'_, Result<(), OutboxError>> {
        let msg_id = id.to_string();
        Box::pin(async move {
            Err(OutboxError::Unsupported(format!(
                "reprocess_failed not supported by this repository backend (id={msg_id})"
            )))
        })
    }

    /// Hard-deletes or archives up to `limit` outbox messages marked as `published`
    /// that were created at or before `older_than_secs` seconds ago.
    /// Returns the number of purged rows.
    fn purge_published(
        &self,
        _older_than_secs: u64,
        _limit: usize,
    ) -> BoxFuture<'_, Result<usize, OutboxError>> {
        Box::pin(async move {
            Err(OutboxError::Unsupported(
                "purge_published not supported by this repository backend".into(),
            ))
        })
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
