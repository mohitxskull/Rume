//! Outbox storage repository and transactional outbox contracts.

use std::fmt::Debug;
use std::future::Future;
use std::pin::Pin;

use rume_data::persistence::backend::{DatabaseBackend, DatabaseTransaction};
use rume_data::persistence::pool::DatabasePool;

use crate::outbox::error::OutboxError;
use crate::outbox::message::OutboxMessage;

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
    ///
    /// This is a required repository method so backends cannot silently fail with
    /// [`OutboxError::Unsupported`] when [`mark_failed`](Self::mark_failed) delegates to it.
    /// Implementations that previously only overrode [`mark_failed`](Self::mark_failed) should
    /// move that logic into `mark_failed_with_max_retries` (honoring `max_retries`) and rely on
    /// the default [`mark_failed`](Self::mark_failed) implementation, which delegates with a
    /// limit of `5`.
    fn mark_failed_with_max_retries(
        &self,
        id: &str,
        lease_token: &str,
        error: String,
        max_retries: u32,
    ) -> BoxFuture<'_, Result<(), OutboxError>>;

    /// Recovers abandoned or timed-out in-flight messages whose claim lease has expired,
    /// incrementing `retry_count` and transitioning rows at or above the default retry limit (`5`)
    /// to `Failed` (otherwise back to `Pending`).
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

    /// Recovers abandoned or timed-out in-flight messages whose claim lease has expired,
    /// incrementing `retry_count` and transitioning rows reaching `max_retries` to `Failed`.
    ///
    /// Defaults to delegating to [`recover_expired_claims`](Self::recover_expired_claims) for
    /// backward compatibility with custom repository implementations.
    fn recover_expired_claims_with_max_retries(
        &self,
        max_lease_age_secs: u64,
        _max_retries: u32,
    ) -> BoxFuture<'_, Result<usize, OutboxError>> {
        self.recover_expired_claims(max_lease_age_secs)
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

impl<B> OutboxRepository for DatabasePool<B>
where
    B: DatabaseBackend + OutboxRepository,
{
    fn save(&self, msg: OutboxMessage) -> BoxFuture<'_, Result<(), OutboxError>> {
        Box::pin(async move {
            let _lease = self
                .acquire_lease_timeout(self.config().connect_timeout())
                .await
                .map_err(|e| OutboxError::Repository(e.to_string()))?;
            self.backend().save(msg).await
        })
    }

    fn fetch_pending(
        &self,
        limit: usize,
    ) -> BoxFuture<'_, Result<Vec<OutboxMessage>, OutboxError>> {
        Box::pin(async move {
            let _lease = self
                .acquire_lease_timeout(self.config().connect_timeout())
                .await
                .map_err(|e| OutboxError::Repository(e.to_string()))?;
            self.backend().fetch_pending(limit).await
        })
    }

    fn claim_pending(
        &self,
        limit: usize,
    ) -> BoxFuture<'_, Result<Vec<OutboxMessage>, OutboxError>> {
        Box::pin(async move {
            let _lease = self
                .acquire_lease_timeout(self.config().connect_timeout())
                .await
                .map_err(|e| OutboxError::Repository(e.to_string()))?;
            self.backend().claim_pending(limit).await
        })
    }

    fn claim_pending_with_lease(
        &self,
        limit: usize,
        lease_duration_secs: u64,
    ) -> BoxFuture<'_, Result<Vec<OutboxMessage>, OutboxError>> {
        Box::pin(async move {
            let _lease = self
                .acquire_lease_timeout(self.config().connect_timeout())
                .await
                .map_err(|e| OutboxError::Repository(e.to_string()))?;
            self.backend()
                .claim_pending_with_lease(limit, lease_duration_secs)
                .await
        })
    }

    fn renew_outbox_lease(
        &self,
        id: &str,
        lease_token: &str,
        extension_secs: u64,
    ) -> BoxFuture<'_, Result<(), OutboxError>> {
        let id = id.to_string();
        let lease_token = lease_token.to_string();
        Box::pin(async move {
            let _lease = self
                .acquire_lease_timeout(self.config().connect_timeout())
                .await
                .map_err(|e| OutboxError::Repository(e.to_string()))?;
            self.backend()
                .renew_outbox_lease(&id, &lease_token, extension_secs)
                .await
        })
    }

    fn mark_published(
        &self,
        id: &str,
        lease_token: &str,
    ) -> BoxFuture<'_, Result<(), OutboxError>> {
        let id = id.to_string();
        let lease_token = lease_token.to_string();
        Box::pin(async move {
            let _lease = self
                .acquire_lease_timeout(self.config().connect_timeout())
                .await
                .map_err(|e| OutboxError::Repository(e.to_string()))?;
            self.backend().mark_published(&id, &lease_token).await
        })
    }

    fn mark_failed(
        &self,
        id: &str,
        lease_token: &str,
        error: String,
    ) -> BoxFuture<'_, Result<(), OutboxError>> {
        self.mark_failed_with_max_retries(id, lease_token, error, 5)
    }

    fn mark_failed_with_max_retries(
        &self,
        id: &str,
        lease_token: &str,
        error: String,
        max_retries: u32,
    ) -> BoxFuture<'_, Result<(), OutboxError>> {
        let id = id.to_string();
        let lease_token = lease_token.to_string();
        Box::pin(async move {
            let _lease = self
                .acquire_lease_timeout(self.config().connect_timeout())
                .await
                .map_err(|e| OutboxError::Repository(e.to_string()))?;
            self.backend()
                .mark_failed_with_max_retries(&id, &lease_token, error, max_retries)
                .await
        })
    }

    fn recover_expired_claims(
        &self,
        max_lease_age_secs: u64,
    ) -> BoxFuture<'_, Result<usize, OutboxError>> {
        Box::pin(async move {
            let _lease = self
                .acquire_lease_timeout(self.config().connect_timeout())
                .await
                .map_err(|e| OutboxError::Repository(e.to_string()))?;
            self.backend()
                .recover_expired_claims(max_lease_age_secs)
                .await
        })
    }

    fn recover_expired_claims_with_max_retries(
        &self,
        max_lease_age_secs: u64,
        max_retries: u32,
    ) -> BoxFuture<'_, Result<usize, OutboxError>> {
        Box::pin(async move {
            let _lease = self
                .acquire_lease_timeout(self.config().connect_timeout())
                .await
                .map_err(|e| OutboxError::Repository(e.to_string()))?;
            self.backend()
                .recover_expired_claims_with_max_retries(max_lease_age_secs, max_retries)
                .await
        })
    }

    fn pending_count(&self) -> BoxFuture<'_, Result<usize, OutboxError>> {
        Box::pin(async move {
            let _lease = self
                .acquire_lease_timeout(self.config().connect_timeout())
                .await
                .map_err(|e| OutboxError::Repository(e.to_string()))?;
            self.backend().pending_count().await
        })
    }

    fn published_count(&self) -> BoxFuture<'_, Result<usize, OutboxError>> {
        Box::pin(async move {
            let _lease = self
                .acquire_lease_timeout(self.config().connect_timeout())
                .await
                .map_err(|e| OutboxError::Repository(e.to_string()))?;
            self.backend().published_count().await
        })
    }

    fn failed_count(&self) -> BoxFuture<'_, Result<usize, OutboxError>> {
        Box::pin(async move {
            let _lease = self
                .acquire_lease_timeout(self.config().connect_timeout())
                .await
                .map_err(|e| OutboxError::Repository(e.to_string()))?;
            self.backend().failed_count().await
        })
    }

    fn fetch_failed(&self, limit: usize) -> BoxFuture<'_, Result<Vec<OutboxMessage>, OutboxError>> {
        Box::pin(async move {
            let _lease = self
                .acquire_lease_timeout(self.config().connect_timeout())
                .await
                .map_err(|e| OutboxError::Repository(e.to_string()))?;
            self.backend().fetch_failed(limit).await
        })
    }

    fn reprocess_failed(&self, id: &str) -> BoxFuture<'_, Result<(), OutboxError>> {
        let id = id.to_string();
        Box::pin(async move {
            let _lease = self
                .acquire_lease_timeout(self.config().connect_timeout())
                .await
                .map_err(|e| OutboxError::Repository(e.to_string()))?;
            self.backend().reprocess_failed(&id).await
        })
    }

    fn purge_published(
        &self,
        older_than_secs: u64,
        limit: usize,
    ) -> BoxFuture<'_, Result<usize, OutboxError>> {
        Box::pin(async move {
            let _lease = self
                .acquire_lease_timeout(self.config().connect_timeout())
                .await
                .map_err(|e| OutboxError::Repository(e.to_string()))?;
            self.backend().purge_published(older_than_secs, limit).await
        })
    }
}

impl<B> TransactionalOutbox<B::Transaction> for DatabasePool<B>
where
    B: DatabaseBackend + TransactionalOutbox<B::Transaction>,
{
    fn save_tx<'a>(
        &'a self,
        tx: &'a B::Transaction,
        message: OutboxMessage,
    ) -> BoxFuture<'a, Result<(), OutboxError>> {
        self.backend().save_tx(tx, message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;

    /// Minimal stub that relies on the default [`OutboxRepository::mark_failed`]
    /// delegation to [`OutboxRepository::mark_failed_with_max_retries`].
    #[derive(Debug, Default)]
    struct StubRepo {
        last_max_retries: AtomicU32,
    }

    impl OutboxRepository for StubRepo {
        fn save(&self, _msg: OutboxMessage) -> BoxFuture<'_, Result<(), OutboxError>> {
            Box::pin(async { Ok(()) })
        }

        fn fetch_pending(
            &self,
            _limit: usize,
        ) -> BoxFuture<'_, Result<Vec<OutboxMessage>, OutboxError>> {
            Box::pin(async { Ok(vec![]) })
        }

        fn claim_pending_with_lease(
            &self,
            _limit: usize,
            _lease_duration_secs: u64,
        ) -> BoxFuture<'_, Result<Vec<OutboxMessage>, OutboxError>> {
            Box::pin(async { Ok(vec![]) })
        }

        fn mark_published(
            &self,
            _id: &str,
            _lease_token: &str,
        ) -> BoxFuture<'_, Result<(), OutboxError>> {
            Box::pin(async { Ok(()) })
        }

        fn mark_failed_with_max_retries(
            &self,
            _id: &str,
            _lease_token: &str,
            _error: String,
            max_retries: u32,
        ) -> BoxFuture<'_, Result<(), OutboxError>> {
            self.last_max_retries.store(max_retries, Ordering::SeqCst);
            Box::pin(async { Ok(()) })
        }

        fn pending_count(&self) -> BoxFuture<'_, Result<usize, OutboxError>> {
            Box::pin(async { Ok(0) })
        }

        fn published_count(&self) -> BoxFuture<'_, Result<usize, OutboxError>> {
            Box::pin(async { Ok(0) })
        }
    }

    #[tokio::test]
    async fn mark_failed_default_delegates_with_default_limit_and_does_not_recurse() {
        let repo = StubRepo::default();
        let res = tokio::time::timeout(
            Duration::from_secs(5),
            repo.mark_failed("id", "token", "boom".into()),
        )
        .await
        .expect("mark_failed default must complete within timeout");
        assert!(res.is_ok(), "mark_failed should succeed, got {res:?}");
        assert_eq!(repo.last_max_retries.load(Ordering::SeqCst), 5);
    }
}
