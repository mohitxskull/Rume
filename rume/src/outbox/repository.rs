//! Outbox storage repository and in-memory test store.

use std::collections::BTreeMap;
use std::fmt::Debug;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, RwLock};

use uuid::Uuid;

use crate::outbox::error::OutboxError;
use crate::outbox::message::{OutboxMessage, OutboxStatus, current_timestamp};
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

    /// Marks a message as successfully published.
    fn mark_published(&self, id: &str) -> BoxFuture<'_, Result<(), OutboxError>>;

    /// Marks a message as failed with an error message and increments its retry counter.
    fn mark_failed(&self, id: &str, error: String) -> BoxFuture<'_, Result<(), OutboxError>>;

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

/// In-memory outbox repository for testing and single-node persistence.
#[derive(Debug, Clone, Default)]
pub struct MemoryOutboxRepository {
    messages: Arc<RwLock<BTreeMap<String, OutboxMessage>>>,
}

impl MemoryOutboxRepository {
    /// Creates a new in-memory outbox repository.
    pub fn new() -> Self {
        Self {
            messages: Arc::new(RwLock::new(BTreeMap::new())),
        }
    }
}

impl OutboxRepository for MemoryOutboxRepository {
    fn save(&self, msg: OutboxMessage) -> BoxFuture<'_, Result<(), OutboxError>> {
        Box::pin(async move {
            let mut messages = self
                .messages
                .write()
                .map_err(|e| OutboxError::Repository(e.to_string()))?;
            messages.insert(msg.id.clone(), msg);
            Ok(())
        })
    }

    fn fetch_pending(
        &self,
        limit: usize,
    ) -> BoxFuture<'_, Result<Vec<OutboxMessage>, OutboxError>> {
        Box::pin(async move {
            let messages = self
                .messages
                .read()
                .map_err(|e| OutboxError::Repository(e.to_string()))?;

            let now = current_timestamp();
            let mut pending: Vec<OutboxMessage> = messages
                .values()
                .filter(|m| {
                    m.status == OutboxStatus::Pending && m.next_retry_at.is_none_or(|t| now >= t)
                })
                .cloned()
                .collect();
            pending.sort_by_key(|m| m.created_at);
            pending.truncate(limit);

            Ok(pending)
        })
    }

    fn claim_pending_with_lease(
        &self,
        limit: usize,
        lease_duration_secs: u64,
    ) -> BoxFuture<'_, Result<Vec<OutboxMessage>, OutboxError>> {
        Box::pin(async move {
            let mut messages = self
                .messages
                .write()
                .map_err(|e| OutboxError::Repository(e.to_string()))?;

            let now = current_timestamp();
            let mut pending_keys: Vec<(u64, String)> = messages
                .values()
                .filter(|m| {
                    m.status == OutboxStatus::Pending && m.next_retry_at.is_none_or(|t| now >= t)
                })
                .map(|m| (m.created_at, m.id.clone()))
                .collect();
            pending_keys.sort_by_key(|(ts, _)| *ts);

            let mut claimed = Vec::new();
            for (_, id) in pending_keys.into_iter().take(limit) {
                if let Some(msg) = messages.get_mut(&id) {
                    msg.status = OutboxStatus::InFlight;
                    msg.lease_token = Some(Uuid::new_v4().to_string());
                    msg.lease_expires_at = Some(now + lease_duration_secs);
                    claimed.push(msg.clone());
                }
            }

            Ok(claimed)
        })
    }

    fn mark_published(&self, id: &str) -> BoxFuture<'_, Result<(), OutboxError>> {
        let id_str = id.to_string();
        Box::pin(async move {
            let mut messages = self
                .messages
                .write()
                .map_err(|e| OutboxError::Repository(e.to_string()))?;

            if let Some(msg) = messages.get_mut(&id_str) {
                msg.status = OutboxStatus::Published;
                msg.last_error = None;
                msg.next_retry_at = None;
                msg.lease_token = None;
                msg.lease_expires_at = None;
            }

            Ok(())
        })
    }

    fn recover_expired_claims(
        &self,
        max_lease_age_secs: u64,
    ) -> BoxFuture<'_, Result<usize, OutboxError>> {
        Box::pin(async move {
            let mut messages = self
                .messages
                .write()
                .map_err(|e| OutboxError::Repository(e.to_string()))?;

            let now = current_timestamp();
            let mut recovered = 0;
            for msg in messages.values_mut() {
                if msg.status == OutboxStatus::InFlight {
                    let expired = match msg.lease_expires_at {
                        Some(exp) => now >= exp,
                        None => now >= msg.created_at + max_lease_age_secs,
                    };
                    if expired {
                        msg.status = OutboxStatus::Pending;
                        msg.lease_token = None;
                        msg.lease_expires_at = None;
                        recovered += 1;
                    }
                }
            }

            Ok(recovered)
        })
    }

    fn mark_failed(&self, id: &str, error: String) -> BoxFuture<'_, Result<(), OutboxError>> {
        let id_str = id.to_string();
        Box::pin(async move {
            let mut messages = self
                .messages
                .write()
                .map_err(|e| OutboxError::Repository(e.to_string()))?;

            if let Some(msg) = messages.get_mut(&id_str) {
                msg.retry_count += 1;
                msg.last_error = Some(error);
                if msg.retry_count >= 5 {
                    msg.status = OutboxStatus::Failed;
                    msg.next_retry_at = None;
                } else {
                    msg.status = OutboxStatus::Pending;
                    let now = current_timestamp();
                    let backoff_secs = 2u64.pow(msg.retry_count.saturating_sub(1));
                    msg.next_retry_at = Some(now + backoff_secs);
                }
            }

            Ok(())
        })
    }

    fn pending_count(&self) -> BoxFuture<'_, Result<usize, OutboxError>> {
        Box::pin(async move {
            let messages = self
                .messages
                .read()
                .map_err(|e| OutboxError::Repository(e.to_string()))?;

            let now = current_timestamp();
            Ok(messages
                .values()
                .filter(|m| {
                    (m.status == OutboxStatus::Pending && m.next_retry_at.is_none_or(|t| now >= t))
                        || m.status == OutboxStatus::InFlight
                })
                .count())
        })
    }

    fn published_count(&self) -> BoxFuture<'_, Result<usize, OutboxError>> {
        Box::pin(async move {
            let messages = self
                .messages
                .read()
                .map_err(|e| OutboxError::Repository(e.to_string()))?;

            Ok(messages
                .values()
                .filter(|m| m.status == OutboxStatus::Published)
                .count())
        })
    }
}
