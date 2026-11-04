//! Background outbox processor and message relay.

use std::collections::BTreeMap;
use std::fmt::Debug;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

use futures_util::FutureExt;
use tokio::time::sleep;

use crate::outbox::error::OutboxError;
use crate::outbox::message::OutboxMessage;
use crate::outbox::repository::{BoxFuture, OutboxRepository};
use crate::routing::{HealthCheck, HealthCheckResult, HealthStatus};

/// Type alias for the message dispatch callback.
pub type MessageDispatcher = Arc<
    dyn Fn(&OutboxMessage) -> BoxFuture<'static, Result<(), OutboxError>> + Send + Sync + 'static,
>;

/// Type alias for a cancellable message dispatch callback that observes a lost-lease cancellation flag.
pub type CancellableMessageDispatcher = Arc<
    dyn Fn(&OutboxMessage, Arc<AtomicBool>) -> BoxFuture<'static, Result<(), OutboxError>>
        + Send
        + Sync
        + 'static,
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

/// Operational health and telemetry summary for an [`OutboxRelay`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RelayStatus {
    /// Whether the relay has been requested to stop.
    pub is_stopping: bool,
    /// Number of consecutive polling or dispatch errors encountered.
    pub consecutive_errors: u32,
    /// Lifetime total polling or repository errors encountered.
    pub total_errors: u64,
    /// Lifetime failed message state transitions (e.g. missing lease tokens or lost leases).
    pub failed_transitions: u64,
    /// Most recent operational error message, if any.
    pub last_error: Option<String>,
    /// Whether the relay is considered healthy (`consecutive_errors < 5`).
    pub is_healthy: bool,
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
    dispatcher: CancellableMessageDispatcher,
    is_stopping: Arc<AtomicBool>,
    config: OutboxRelayConfig,
    consecutive_errors: Arc<AtomicU32>,
    total_errors: Arc<AtomicU64>,
    failed_transitions: Arc<AtomicU64>,
    last_error: Arc<RwLock<Option<String>>>,
    shutdown_notify: Arc<tokio::sync::Notify>,
}

impl<R: OutboxRepository> Clone for OutboxRelay<R> {
    fn clone(&self) -> Self {
        Self {
            repository: Arc::clone(&self.repository),
            dispatcher: Arc::clone(&self.dispatcher),
            is_stopping: Arc::clone(&self.is_stopping),
            config: self.config.clone(),
            consecutive_errors: Arc::clone(&self.consecutive_errors),
            total_errors: Arc::clone(&self.total_errors),
            failed_transitions: Arc::clone(&self.failed_transitions),
            last_error: Arc::clone(&self.last_error),
            shutdown_notify: Arc::clone(&self.shutdown_notify),
        }
    }
}

impl<R: OutboxRepository> Debug for OutboxRelay<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutboxRelay")
            .field("is_stopping", &self.is_stopping.load(Ordering::Relaxed))
            .field("status", &self.status())
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
            dispatcher: Arc::new(move |msg, _cancel| Box::pin(dispatcher(msg))),
            is_stopping: Arc::new(AtomicBool::new(false)),
            config: OutboxRelayConfig::default(),
            consecutive_errors: Arc::new(AtomicU32::new(0)),
            total_errors: Arc::new(AtomicU64::new(0)),
            failed_transitions: Arc::new(AtomicU64::new(0)),
            last_error: Arc::new(RwLock::new(None)),
            shutdown_notify: Arc::new(tokio::sync::Notify::new()),
        }
    }

    /// Creates a new outbox relay with a cancellable message dispatcher that receives
    /// an `Arc<AtomicBool>` signal set to true if lease ownership is lost during dispatch.
    pub fn new_cancellable<F, Fut>(repository: Arc<R>, dispatcher: F) -> Self
    where
        F: Fn(&OutboxMessage, Arc<AtomicBool>) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<(), OutboxError>> + Send + 'static,
    {
        Self {
            repository,
            dispatcher: Arc::new(move |msg, cancel| Box::pin(dispatcher(msg, cancel))),
            is_stopping: Arc::new(AtomicBool::new(false)),
            config: OutboxRelayConfig::default(),
            consecutive_errors: Arc::new(AtomicU32::new(0)),
            total_errors: Arc::new(AtomicU64::new(0)),
            failed_transitions: Arc::new(AtomicU64::new(0)),
            last_error: Arc::new(RwLock::new(None)),
            shutdown_notify: Arc::new(tokio::sync::Notify::new()),
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
        // Propagate errors directly to prevent silent lease stalls or misconfigured repositories.
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
            let lease_lost = Arc::new(AtomicBool::new(false));
            let lease_lost_hb = Arc::clone(&lease_lost);

            let heartbeat_handle = tokio::spawn(async move {
                let mut interval = tokio::time::interval(hb_interval);
                interval.tick().await; // first tick immediate
                loop {
                    tokio::select! {
                        _ = &mut cancel_rx => break,
                        _ = interval.tick() => {
                            if let Err(e) = hb_repo.renew_outbox_lease(&hb_msg_id, &hb_token, renew_extension_secs).await {
                                tracing::error!(
                                    message_id = %hb_msg_id,
                                    error = %e,
                                    "Failed to renew outbox lease heartbeat; lease ownership lost during dispatch"
                                );
                                lease_lost_hb.store(true, Ordering::SeqCst);
                                break;
                            }
                        }
                    }
                }
            });

            let cancel_signal = Arc::clone(&lease_lost);
            let fut_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                (self.dispatcher)(&message, cancel_signal)
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

            // If heartbeat failed mid-dispatch, ownership was lost. Propagate this failure
            // to RelayPollResult and skip ack/nack to avoid corrupted state transitions.
            if lease_lost.load(Ordering::SeqCst) {
                tracing::error!(
                    message_id = %message.id,
                    "Outbox message lease was lost during active dispatch; marking as failed transition"
                );
                result.failed_transitions += 1;
                continue;
            }

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

    /// Returns the current runtime health and error telemetry for this relay.
    pub fn status(&self) -> RelayStatus {
        let consecutive = self.consecutive_errors.load(Ordering::Relaxed);
        let last_err = self.last_error.read().ok().and_then(|g| g.clone());
        RelayStatus {
            is_stopping: self.is_stopping.load(Ordering::Relaxed),
            consecutive_errors: consecutive,
            total_errors: self.total_errors.load(Ordering::Relaxed),
            failed_transitions: self.failed_transitions.load(Ordering::Relaxed),
            last_error: last_err,
            is_healthy: consecutive < 5,
        }
    }

    /// Creates a Rume [`HealthCheck`] for supervising this relay's operational health.
    pub fn health_check(&self, name: impl Into<String>) -> RelayHealthCheck<R> {
        RelayHealthCheck {
            name: name.into(),
            relay: self.clone(),
        }
    }

    /// Spawns the background polling loop using configured interval and batch size.
    ///
    /// ## Supervision & Health Guarantees
    /// The returned [`tokio::task::JoinHandle`] supervises background task lifecycle (e.g., detecting panics
    /// or explicitly aborting the polling task). The polling loop treats transient database or broker errors
    /// as retryable conditions, continuing to poll without terminating the task.
    ///
    /// To monitor operational health, inspect [`OutboxRelay::status`] or register [`OutboxRelay::health_check`].
    pub fn start_with_config(&self) -> tokio::task::JoinHandle<()> {
        let relay = self.clone();
        tokio::spawn(async move {
            let interval = relay.config.poll_interval;
            let batch = relay.config.batch_size;
            while !relay.is_stopping.load(Ordering::Relaxed) {
                match relay.poll_and_dispatch(batch).await {
                    Ok(res) => {
                        relay.consecutive_errors.store(0, Ordering::SeqCst);
                        if res.failed_transitions > 0 {
                            relay
                                .failed_transitions
                                .fetch_add(res.failed_transitions as u64, Ordering::SeqCst);
                            tracing::error!(
                                published = res.published,
                                retried = res.retried,
                                failed_transitions = res.failed_transitions,
                                "Outbox relay encountered failed message state transitions"
                            );
                        }
                    }
                    Err(err) => {
                        relay.consecutive_errors.fetch_add(1, Ordering::SeqCst);
                        relay.total_errors.fetch_add(1, Ordering::SeqCst);
                        if let Ok(mut g) = relay.last_error.write() {
                            *g = Some(err.to_string());
                        }
                        tracing::error!(error = %err, "Outbox relay background dispatch error");
                    }
                }
                tokio::select! {
                    _ = sleep(interval) => {},
                    _ = relay.shutdown_notify.notified() => { break; }
                }
            }
        })
    }

    /// Spawns the background polling loop with explicit interval.
    ///
    /// Returns a [`tokio::task::JoinHandle`] enabling background task supervision and cancellation.
    pub fn start(&self, interval: Duration) -> tokio::task::JoinHandle<()> {
        let mut config = self.config.clone();
        config.poll_interval = interval;
        let mut relay = self.clone();
        relay.config = config;
        relay.start_with_config()
    }

    /// Requests shutdown and drains remaining pending messages within `timeout`.
    ///
    /// Returns `Ok(true)` if all messages were drained, `Ok(false)` if the drain
    /// timed out, or `Err(err)` if an unrecoverable repository error occurred while
    /// checking pending count or dispatching messages.
    pub async fn try_drain(&self, timeout: Duration) -> Result<bool, OutboxError> {
        self.is_stopping.store(true, Ordering::SeqCst);
        self.shutdown_notify.notify_waiters();
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

/// Health check implementation for an [`OutboxRelay`].
pub struct RelayHealthCheck<R: OutboxRepository> {
    name: String,
    relay: OutboxRelay<R>,
}

impl<R: OutboxRepository> Clone for RelayHealthCheck<R> {
    fn clone(&self) -> Self {
        Self {
            name: self.name.clone(),
            relay: self.relay.clone(),
        }
    }
}

impl<R: OutboxRepository> Debug for RelayHealthCheck<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RelayHealthCheck")
            .field("name", &self.name)
            .field("status", &self.relay.status())
            .finish()
    }
}

impl<R: OutboxRepository> HealthCheck for RelayHealthCheck<R> {
    fn name(&self) -> &str {
        &self.name
    }

    fn run(&self) -> Pin<Box<dyn std::future::Future<Output = HealthCheckResult> + Send>> {
        let name = self.name.clone();
        let relay = self.relay.clone();

        Box::pin(async move {
            let status = relay.status();
            let mut meta = BTreeMap::new();
            meta.insert(
                "is_stopping".to_string(),
                serde_json::json!(status.is_stopping),
            );
            meta.insert(
                "consecutive_errors".to_string(),
                serde_json::json!(status.consecutive_errors),
            );
            meta.insert(
                "total_errors".to_string(),
                serde_json::json!(status.total_errors),
            );
            meta.insert(
                "failed_transitions".to_string(),
                serde_json::json!(status.failed_transitions),
            );
            meta.insert("is_healthy".to_string(), serde_json::json!(status.is_healthy));
            if let Some(err) = &status.last_error {
                meta.insert("last_error".to_string(), serde_json::json!(err));
            }

            if status.is_stopping {
                HealthCheckResult {
                    name,
                    status: HealthStatus::Warning,
                    message: "outbox relay is stopping".to_string(),
                    meta,
                    cached: false,
                }
            } else if status.consecutive_errors >= 5 {
                HealthCheckResult {
                    name,
                    status: HealthStatus::Failed,
                    message: format!(
                        "outbox relay exceeded error threshold (consecutive_errors: {})",
                        status.consecutive_errors
                    ),
                    meta,
                    cached: false,
                }
            } else if status.consecutive_errors > 0 {
                HealthCheckResult {
                    name,
                    status: HealthStatus::Warning,
                    message: format!(
                        "outbox relay experiencing transient errors: {}",
                        status.last_error.as_deref().unwrap_or("unknown")
                    ),
                    meta,
                    cached: false,
                }
            } else {
                HealthCheckResult {
                    name,
                    status: HealthStatus::Ok,
                    message: "outbox relay healthy".to_string(),
                    meta,
                    cached: false,
                }
            }
        })
    }
}
