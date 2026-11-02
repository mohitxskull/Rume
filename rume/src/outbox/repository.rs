//! Outbox storage repository and in-memory test store.

use std::collections::BTreeMap;
use std::fmt::Debug;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, RwLock};

use crate::outbox::error::OutboxError;
use crate::outbox::message::{OutboxMessage, OutboxStatus};

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
    /// to `InFlight` and ordering by creation timestamp.
    fn claim_pending(&self, limit: usize)
    -> BoxFuture<'_, Result<Vec<OutboxMessage>, OutboxError>>;

    /// Marks a message as successfully published.
    fn mark_published(&self, id: &str) -> BoxFuture<'_, Result<(), OutboxError>>;

    /// Marks a message as failed with an error message and increments its retry counter.
    fn mark_failed(&self, id: &str, error: String) -> BoxFuture<'_, Result<(), OutboxError>>;

    /// Returns the number of currently pending outbox messages.
    fn pending_count(&self) -> BoxFuture<'_, Result<usize, OutboxError>>;

    /// Returns the number of successfully published messages.
    fn published_count(&self) -> BoxFuture<'_, Result<usize, OutboxError>>;
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

            let mut pending: Vec<OutboxMessage> = messages
                .values()
                .filter(|m| m.status == OutboxStatus::Pending)
                .cloned()
                .collect();
            pending.sort_by_key(|m| m.created_at);
            pending.truncate(limit);

            Ok(pending)
        })
    }

    fn claim_pending(
        &self,
        limit: usize,
    ) -> BoxFuture<'_, Result<Vec<OutboxMessage>, OutboxError>> {
        Box::pin(async move {
            let mut messages = self
                .messages
                .write()
                .map_err(|e| OutboxError::Repository(e.to_string()))?;

            let mut pending_keys: Vec<(u64, String)> = messages
                .values()
                .filter(|m| m.status == OutboxStatus::Pending)
                .map(|m| (m.created_at, m.id.clone()))
                .collect();
            pending_keys.sort_by_key(|(ts, _)| *ts);

            let mut claimed = Vec::new();
            for (_, id) in pending_keys.into_iter().take(limit) {
                if let Some(msg) = messages.get_mut(&id) {
                    msg.status = OutboxStatus::InFlight;
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
            }

            Ok(())
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
                } else {
                    msg.status = OutboxStatus::Pending;
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

            Ok(messages
                .values()
                .filter(|m| m.status == OutboxStatus::Pending || m.status == OutboxStatus::InFlight)
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
