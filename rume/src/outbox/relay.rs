//! Background outbox processor and message relay.

use std::fmt::Debug;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures_util::FutureExt;
use tokio::time::sleep;

use crate::outbox::error::OutboxError;
use crate::outbox::message::OutboxMessage;
use crate::outbox::repository::{BoxFuture, OutboxRepository};

/// Type alias for the message dispatch callback.
pub type MessageDispatcher = Arc<
    dyn Fn(&OutboxMessage) -> BoxFuture<'static, Result<(), OutboxError>> + Send + Sync + 'static,
>;

/// Configuration for the outbox relay background processor.
#[derive(Debug, Clone)]
pub struct OutboxRelayConfig {
    /// Maximum number of messages claimed and dispatched per batch.
    pub batch_size: usize,
    /// Duration in seconds for which claimed messages are leased to this relay.
    pub lease_duration_secs: u64,
    /// Polling interval between relay loop passes.
    pub poll_interval: Duration,
}

impl Default for OutboxRelayConfig {
    fn default() -> Self {
        Self {
            batch_size: 50,
            lease_duration_secs: 30,
            poll_interval: Duration::from_millis(500),
        }
    }
}

/// Background relay that drains pending outbox messages and dispatches them to an external broker or event bus.
pub struct OutboxRelay<R: OutboxRepository> {
    repository: Arc<R>,
    dispatcher: MessageDispatcher,
    is_stopping: Arc<AtomicBool>,
    config: OutboxRelayConfig,
}

impl<R: OutboxRepository> Clone for OutboxRelay<R> {
    fn clone(&self) -> Self {
        Self {
            repository: Arc::clone(&self.repository),
            dispatcher: Arc::clone(&self.dispatcher),
            is_stopping: Arc::clone(&self.is_stopping),
            config: self.config.clone(),
        }
    }
}

impl<R: OutboxRepository> Debug for OutboxRelay<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutboxRelay")
            .field("is_stopping", &self.is_stopping.load(Ordering::Relaxed))
            .field("config", &self.config)
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
            config: OutboxRelayConfig::default(),
        }
    }

    /// Configures the outbox relay parameters.
    pub fn with_config(mut self, config: OutboxRelayConfig) -> Self {
        self.config = config;
        self
    }

    /// Claims pending messages atomically and dispatches them in order.
    pub async fn poll_and_dispatch(&self, batch_size: usize) -> Result<usize, OutboxError> {
        // Automatically reclaim any expired in-flight message leases before polling new items
        if let Err(err) = self
            .repository
            .recover_expired_claims(self.config.lease_duration_secs)
            .await
        {
            tracing::warn!(error = %err, "Failed to recover expired outbox claims");
        }

        let pending = self
            .repository
            .claim_pending_with_lease(batch_size, self.config.lease_duration_secs)
            .await?;
        let mut published_count = 0;

        for message in pending {
            let token = match message.lease_token.as_deref() {
                Some(t) => t,
                None => {
                    tracing::error!(message_id = %message.id, "Claimed outbox message is missing lease token");
                    continue;
                }
            };

            let fut_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                (self.dispatcher)(&message)
            }));

            let dispatch_result = match fut_result {
                Ok(fut) => match std::panic::AssertUnwindSafe(fut).catch_unwind().await {
                    Ok(res) => res,
                    Err(_) => Err(OutboxError::Dispatch("outbox dispatcher panicked".into())),
                },
                Err(_) => Err(OutboxError::Dispatch("outbox dispatcher panicked".into())),
            };

            match dispatch_result {
                Ok(()) => {
                    if let Err(err) = self.repository.mark_published(&message.id, token).await {
                        tracing::error!(message_id = %message.id, error = %err, "Failed to mark outbox message as published");
                    } else {
                        published_count += 1;
                    }
                }
                Err(err) => {
                    if let Err(mark_err) = self
                        .repository
                        .mark_failed(&message.id, token, err.to_string())
                        .await
                    {
                        tracing::error!(
                            message_id = %message.id,
                            error = %mark_err,
                            "Failed to mark outbox message as failed"
                        );
                    }
                }
            }
        }

        Ok(published_count)
    }

    /// Spawns the background polling loop using configured interval and batch size.
    pub fn start_with_config(&self) {
        let relay = self.clone();
        tokio::spawn(async move {
            let interval = relay.config.poll_interval;
            let batch = relay.config.batch_size;
            while !relay.is_stopping.load(Ordering::Relaxed) {
                if let Err(err) = relay.poll_and_dispatch(batch).await {
                    tracing::error!(error = %err, "Outbox relay background dispatch error");
                }
                sleep(interval).await;
            }
        });
    }

    /// Spawns the background polling loop with explicit interval.
    pub fn start(&self, interval: Duration) {
        let relay = self.clone();
        let batch = relay.config.batch_size;
        tokio::spawn(async move {
            while !relay.is_stopping.load(Ordering::Relaxed) {
                if let Err(err) = relay.poll_and_dispatch(batch).await {
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
