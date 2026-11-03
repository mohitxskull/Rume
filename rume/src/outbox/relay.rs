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

/// Result of an outbox relay polling pass, reporting counts for published messages,
/// retry-scheduled messages, and failed state transitions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RelayPollResult {
    /// Number of claimed messages successfully dispatched and transitioned to published status.
    pub published: usize,
    /// Number of claimed messages whose dispatch failed and retry/dead-failure was scheduled.
    pub retried: usize,
    /// Number of messages where repository state transition failed (e.g. lease expired or database failure).
    pub failed_transitions: usize,
}

impl RelayPollResult {
    /// Returns true if no state transition failures occurred during this polling pass.
    pub fn is_success(&self) -> bool {
        self.failed_transitions == 0
    }

    /// Returns the total number of claimed messages processed during this pass.
    pub fn total_processed(&self) -> usize {
        self.published + self.retried + self.failed_transitions
    }
}

impl std::ops::Deref for RelayPollResult {
    type Target = usize;
    fn deref(&self) -> &Self::Target {
        &self.published
    }
}

impl PartialEq<usize> for RelayPollResult {
    fn eq(&self, other: &usize) -> bool {
        self.published == *other && self.failed_transitions == 0
    }
}

impl PartialEq<RelayPollResult> for usize {
    fn eq(&self, other: &RelayPollResult) -> bool {
        *self == other.published && other.failed_transitions == 0
    }
}

/// Configuration for the outbox relay background processor.
#[derive(Debug, Clone)]
pub struct OutboxRelayConfig {
    /// Maximum number of messages claimed and dispatched per batch.
    pub batch_size: usize,
    /// Duration in seconds for which claimed messages are leased to this relay.
    pub lease_duration_secs: u64,
    /// Polling interval between relay loop passes.
    pub poll_interval: Duration,
    /// Maximum number of dispatch retries before transitioning a message to failed.
    pub max_retries: u32,
}

impl Default for OutboxRelayConfig {
    fn default() -> Self {
        Self {
            batch_size: 50,
            lease_duration_secs: 30,
            poll_interval: Duration::from_millis(500),
            max_retries: 5,
        }
    }
}

impl OutboxRelayConfig {
    /// Sets a custom retry limit before a message is marked as failed.
    pub fn with_max_retries(mut self, max_retries: u32) -> Self {
        self.max_retries = max_retries;
        self
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
    pub async fn poll_and_dispatch(&self, batch_size: usize) -> Result<RelayPollResult, OutboxError> {
        // Automatically reclaim any expired in-flight message leases before polling new items.
        // Propagate errors directly so callers/operators are aware of database failures.
        self.repository
            .recover_expired_claims(self.config.lease_duration_secs)
            .await?;

        let pending = self
            .repository
            .claim_pending_with_lease(batch_size, self.config.lease_duration_secs)
            .await?;
        let mut result = RelayPollResult::default();

        for message in pending {
            let token = match message.lease_token.as_deref() {
                Some(t) => t,
                None => {
                    tracing::error!(message_id = %message.id, "Claimed outbox message is missing lease token");
                    result.failed_transitions += 1;
                    continue;
                }
            };

            // Renew the lease before dispatching to refresh it after waiting in batch
            let lease_duration_secs = self.config.lease_duration_secs;
            let renew_extension_secs = lease_duration_secs.max(2);
            if let Err(e) = self
                .repository
                .renew_outbox_lease(&message.id, token, renew_extension_secs)
                .await
            {
                tracing::warn!(
                    message_id = %message.id,
                    error = %e,
                    "Failed to renew outbox message lease before dispatch; lease expired or token mismatch"
                );
                result.failed_transitions += 1;
                continue;
            }

            // Spawn a heartbeat task to keep the lease renewed while the dispatcher runs
            let hb_repo = Arc::clone(&self.repository);
            let hb_msg_id = message.id.clone();
            let hb_token = token.to_string();
            let hb_interval = Duration::from_millis(
                (lease_duration_secs * 1000 / 3).clamp(50, 20_000),
            );
            let (cancel_tx, mut cancel_rx) = tokio::sync::oneshot::channel::<()>();

            let heartbeat_handle = tokio::spawn(async move {
                let mut interval = tokio::time::interval(hb_interval);
                interval.tick().await; // first tick immediate
                loop {
                    tokio::select! {
                        _ = &mut cancel_rx => break,
                        _ = interval.tick() => {
                            if let Err(e) = hb_repo.renew_outbox_lease(&hb_msg_id, &hb_token, renew_extension_secs).await {
                                tracing::warn!(
                                    message_id = %hb_msg_id,
                                    error = %e,
                                    "Failed to renew outbox lease heartbeat"
                                );
                                break;
                            }
                        }
                    }
                }
            });

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

            let _ = cancel_tx.send(());
            let _ = heartbeat_handle.await;

            match dispatch_result {
                Ok(()) => {
                    match self.repository.mark_published(&message.id, token).await {
                        Ok(()) => {
                            result.published += 1;
                        }
                        Err(err) => {
                            tracing::error!(
                                message_id = %message.id,
                                error = %err,
                                "Failed to mark outbox message as published"
                            );
                            result.failed_transitions += 1;
                        }
                    }
                }
                Err(err) => {
                    match self
                        .repository
                        .mark_failed_with_max_retries(
                            &message.id,
                            token,
                            err.to_string(),
                            self.config.max_retries,
                        )
                        .await
                    {
                        Ok(()) => {
                            result.retried += 1;
                        }
                        Err(mark_err) => {
                            tracing::error!(
                                message_id = %message.id,
                                error = %mark_err,
                                "Failed to mark outbox message as failed"
                            );
                            result.failed_transitions += 1;
                        }
                    }
                }
            }
        }

        Ok(result)
    }

    /// Spawns the background polling loop using configured interval and batch size.
    pub fn start_with_config(&self) {
        let relay = self.clone();
        tokio::spawn(async move {
            let interval = relay.config.poll_interval;
            let batch = relay.config.batch_size;
            while !relay.is_stopping.load(Ordering::Relaxed) {
                match relay.poll_and_dispatch(batch).await {
                    Ok(res) => {
                        if !res.is_success() {
                            tracing::error!(
                                published = res.published,
                                retried = res.retried,
                                failed_transitions = res.failed_transitions,
                                "Outbox relay encountered failed message state transitions"
                            );
                        }
                    }
                    Err(err) => {
                        tracing::error!(error = %err, "Outbox relay background dispatch error");
                    }
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
                match relay.poll_and_dispatch(batch).await {
                    Ok(res) => {
                        if !res.is_success() {
                            tracing::error!(
                                published = res.published,
                                retried = res.retried,
                                failed_transitions = res.failed_transitions,
                                "Outbox relay encountered failed message state transitions"
                            );
                        }
                    }
                    Err(err) => {
                        tracing::error!(error = %err, "Outbox relay background dispatch error");
                    }
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
            let res = self.poll_and_dispatch(50).await?;
            if !res.is_success() {
                return Err(OutboxError::Repository(format!(
                    "outbox relay encountered {} failed state transitions during drain",
                    res.failed_transitions
                )));
            }
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
