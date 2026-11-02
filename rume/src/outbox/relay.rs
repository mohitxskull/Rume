//! Background outbox processor and message relay.

use std::fmt::Debug;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::time::sleep;

use crate::outbox::error::OutboxError;
use crate::outbox::message::OutboxMessage;
use crate::outbox::repository::{BoxFuture, OutboxRepository};

/// Type alias for the message dispatch callback.
pub type MessageDispatcher = Arc<
    dyn Fn(&OutboxMessage) -> BoxFuture<'static, Result<(), OutboxError>> + Send + Sync + 'static,
>;

/// Background relay that drains pending outbox messages and dispatches them to an external broker or event bus.
pub struct OutboxRelay<R: OutboxRepository> {
    repository: Arc<R>,
    dispatcher: MessageDispatcher,
    is_stopping: Arc<AtomicBool>,
}

impl<R: OutboxRepository> Clone for OutboxRelay<R> {
    fn clone(&self) -> Self {
        Self {
            repository: Arc::clone(&self.repository),
            dispatcher: Arc::clone(&self.dispatcher),
            is_stopping: Arc::clone(&self.is_stopping),
        }
    }
}

impl<R: OutboxRepository> Debug for OutboxRelay<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutboxRelay")
            .field("is_stopping", &self.is_stopping.load(Ordering::Relaxed))
            .finish()
    }
}

impl<R: OutboxRepository> OutboxRelay<R> {
    /// Creates a new outbox relay with the given repository and message dispatcher closure.
    pub fn new<F, Fut>(repository: Arc<R>, dispatcher: F) -> Self
    where
        F: Fn(&OutboxMessage) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<(), OutboxError>> + Send + 'static,
    {
        Self {
            repository,
            dispatcher: Arc::new(move |msg| Box::pin(dispatcher(msg))),
            is_stopping: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Claims pending messages atomically and dispatches them in order.
    pub async fn poll_and_dispatch(&self, batch_size: usize) -> Result<usize, OutboxError> {
        // Automatically reclaim any expired in-flight message leases before polling new items
        let _ = self.repository.recover_expired_claims(30).await;

        let pending = self.repository.claim_pending(batch_size).await?;
        let mut published_count = 0;

        for message in pending {
            match (self.dispatcher)(&message).await {
                Ok(()) => {
                    self.repository.mark_published(&message.id).await?;
                    published_count += 1;
                }
                Err(err) => {
                    self.repository
                        .mark_failed(&message.id, err.to_string())
                        .await?;
                }
            }
        }

        Ok(published_count)
    }

    /// Spawns the background polling loop.
    pub fn start(&self, interval: Duration) {
        let relay = self.clone();
        tokio::spawn(async move {
            while !relay.is_stopping.load(Ordering::Relaxed) {
                if let Err(err) = relay.poll_and_dispatch(50).await {
                    tracing::error!(error = %err, "Outbox relay background dispatch error");
                }
                sleep(interval).await;
            }
        });
    }

    /// Requests shutdown and drains remaining pending messages within `timeout`.
    ///
    /// Returns `Ok(true)` if all messages were drained, `Ok(false)` if the drain
    /// timed out, or `Err(err)` if an unrecoverable repository error occurred while
    /// checking pending count or dispatching messages.
    pub async fn try_drain(&self, timeout: Duration) -> Result<bool, OutboxError> {
        self.is_stopping.store(true, Ordering::SeqCst);
        let start = std::time::Instant::now();

        loop {
            let count = self.repository.pending_count().await?;
            if count == 0 {
                return Ok(true);
            }
            if start.elapsed() >= timeout {
                tracing::warn!(
                    remaining = count,
                    "Outbox relay drain timed out with messages remaining"
                );
                return Ok(false);
            }
            self.poll_and_dispatch(50).await?;
            sleep(Duration::from_millis(20)).await;
        }
    }

    /// Requests shutdown and drains remaining pending messages within `timeout`.
    ///
    /// Returns `true` if all pending messages were drained, or `false` on timeout or repository error.
    pub async fn drain(&self, timeout: Duration) -> bool {
        match self.try_drain(timeout).await {
            Ok(drained) => drained,
            Err(err) => {
                tracing::error!(error = %err, "Outbox relay failed to drain due to repository error");
                false
            }
        }
    }
}
