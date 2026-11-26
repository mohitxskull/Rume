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
use rume_foundation::health::{HealthCheck, HealthCheckResult, HealthStatus};

/// Type alias for a cancellable message dispatch callback that observes a lost-lease cancellation flag.
pub type CancellableMessageDispatcher = Arc<
    dyn Fn(&OutboxMessage, Arc<AtomicBool>) -> BoxFuture<'static, Result<(), OutboxError>>
        + Send
        + Sync
        + 'static,
>;

/// Builds a dispatcher closure that publishes [`OutboxMessage`]s to any [`BrokerPublisher`]
/// with `message_id`, `event_type`, and W3C `traceparent`/`tracestate` headers.
pub fn broker_dispatcher(
    broker: Arc<dyn crate::broker::BrokerPublisher>,
) -> impl Fn(&OutboxMessage) -> BoxFuture<'static, Result<(), OutboxError>> + Send + Sync + 'static
{
    move |msg: &OutboxMessage| {
        let broker = Arc::clone(&broker);
        let mut headers = BTreeMap::new();
        headers.insert("message_id".to_string(), msg.id.clone());
        headers.insert("event_type".to_string(), msg.event_type.clone());
        let trace_ctx = rume_observability::TraceContext::current().or_else(|| {
            msg.trace_context
                .as_deref()
                .and_then(rume_observability::TraceContext::parse_serialized)
        });
        if let Some(tc) = trace_ctx {
            headers.insert("traceparent".to_string(), tc.traceparent);
            if let Some(ts) = tc.tracestate {
                headers.insert("tracestate".to_string(), ts);
            }
        }
        let broker_msg = crate::broker::BrokerMessage {
            topic: msg.event_type.clone(),
            payload: msg.payload.clone(),
            headers,
        };
        Box::pin(async move {
            broker
                .publish(broker_msg)
                .await
                .map_err(|e| OutboxError::Dispatch(e.to_string()))
        })
    }
}

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
    /// Lifetime total published messages hard-deleted or archived by the background sweeper.
    pub total_purged: u64,
    /// Most recent operational error message, if any.
    pub last_error: Option<String>,
    /// Whether the relay is considered healthy (`consecutive_errors < 5`).
    pub is_healthy: bool,
}

/// Configuration for the outbox published message sweeper.
#[derive(Debug, Clone)]
pub struct OutboxSweeperConfig {
    /// Retention age in seconds: messages marked as published older than this are purged.
    pub retention_secs: u64,
    /// Maximum number of messages to purge in a single batch pass.
    pub batch_size: usize,
    /// Interval between background sweeper passes.
    pub interval: Duration,
}

impl Default for OutboxSweeperConfig {
    fn default() -> Self {
        Self {
            retention_secs: 86400, // 24 hours
            batch_size: 500,
            interval: Duration::from_secs(300), // 5 minutes
        }
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
    dispatcher: CancellableMessageDispatcher,
    is_stopping: Arc<AtomicBool>,
    config: OutboxRelayConfig,
    consecutive_errors: Arc<AtomicU32>,
    total_errors: Arc<AtomicU64>,
    failed_transitions: Arc<AtomicU64>,
    total_purged: Arc<AtomicU64>,
    last_error: Arc<RwLock<Option<String>>>,
    // Why: `watch::Sender<bool>` latches shutdown state (`false` -> `true`) so
    // `start_sweeper` and `start_with_config` cannot miss a shutdown signal sent
    // while `sweep_published` or `poll_and_dispatch` is in-flight.
    shutdown_tx: Arc<tokio::sync::watch::Sender<bool>>,
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
            total_purged: Arc::clone(&self.total_purged),
            last_error: Arc::clone(&self.last_error),
            shutdown_tx: Arc::clone(&self.shutdown_tx),
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
        let (shutdown_tx, _rx) = tokio::sync::watch::channel(false);
        Self {
            repository,
            dispatcher: Arc::new(move |msg, _cancel| Box::pin(dispatcher(msg))),
            is_stopping: Arc::new(AtomicBool::new(false)),
            config: OutboxRelayConfig::default(),
            consecutive_errors: Arc::new(AtomicU32::new(0)),
            total_errors: Arc::new(AtomicU64::new(0)),
            failed_transitions: Arc::new(AtomicU64::new(0)),
            total_purged: Arc::new(AtomicU64::new(0)),
            last_error: Arc::new(RwLock::new(None)),
            shutdown_tx: Arc::new(shutdown_tx),
        }
    }

    /// Creates a new outbox relay with a cancellable message dispatcher that receives
    /// an `Arc<AtomicBool>` signal set to true if lease ownership is lost during dispatch.
    pub fn new_cancellable<F, Fut>(repository: Arc<R>, dispatcher: F) -> Self
    where
        F: Fn(&OutboxMessage, Arc<AtomicBool>) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<(), OutboxError>> + Send + 'static,
    {
        let (shutdown_tx, _rx) = tokio::sync::watch::channel(false);
        Self {
            repository,
            dispatcher: Arc::new(move |msg, cancel| Box::pin(dispatcher(msg, cancel))),
            is_stopping: Arc::new(AtomicBool::new(false)),
            config: OutboxRelayConfig::default(),
            consecutive_errors: Arc::new(AtomicU32::new(0)),
            total_errors: Arc::new(AtomicU64::new(0)),
            failed_transitions: Arc::new(AtomicU64::new(0)),
            total_purged: Arc::new(AtomicU64::new(0)),
            last_error: Arc::new(RwLock::new(None)),
            shutdown_tx: Arc::new(shutdown_tx),
        }
    }

    /// Configures the outbox relay parameters.
    pub fn with_config(mut self, config: OutboxRelayConfig) -> Self {
        self.config = config;
        self
    }

    /// Creates a new outbox relay that publishes claimed outbox messages to `broker`.
    pub fn with_broker(
        repository: Arc<R>,
        broker: Arc<dyn crate::broker::BrokerPublisher>,
    ) -> Self {
        Self::new(repository, broker_dispatcher(broker))
    }

    /// Claims pending messages atomically and dispatches them in order.
    pub async fn poll_and_dispatch(
        &self,
        batch_size: usize,
    ) -> Result<RelayPollResult, OutboxError> {
        // Automatically reclaim any expired in-flight message leases before polling new items.
        // Propagate errors directly to prevent silent lease stalls or misconfigured repositories.
        self.repository
            .recover_expired_claims_with_max_retries(
                self.config.lease_duration_secs,
                self.config.max_retries,
            )
            .await?;

        let pending = self
            .repository
            .claim_pending_with_lease(batch_size, self.config.lease_duration_secs)
            .await?;
        let mut result = RelayPollResult::default();
        if pending.is_empty() {
            return Ok(result);
        }

        let lease_duration_secs = self.config.lease_duration_secs;
        let renew_extension_secs = lease_duration_secs.max(2);
        let hb_interval = Duration::from_millis((lease_duration_secs * 1000 / 3).clamp(50, 20_000));

        // Why: heartbeat ALL remaining claimed messages in the batch (not just the
        // currently dispatching one) so slow earlier dispatches cannot starve later
        // messages in the same batch into lease expiry and duplicate reclamation.
        let batch_leases: Arc<Vec<Option<(String, String)>>> = Arc::new(
            pending
                .iter()
                .map(|m| m.lease_token.as_ref().map(|t| (m.id.clone(), t.clone())))
                .collect(),
        );
        let lease_lost_flags: Arc<Vec<Arc<AtomicBool>>> = Arc::new(
            (0..pending.len())
                .map(|_| Arc::new(AtomicBool::new(false)))
                .collect(),
        );
        // Tracks the first index in `pending` that has not yet been finalized; guarded by a
        // mutex so advancing `next_idx` before `mark_published`/`mark_failed` never races with
        // an in-flight heartbeat renewal tick for that message.
        let batch_cursor = Arc::new(tokio::sync::Mutex::new(0usize));
        let (cancel_tx, mut cancel_rx) = tokio::sync::oneshot::channel::<()>();

        let hb_repo = Arc::clone(&self.repository);
        let hb_leases = Arc::clone(&batch_leases);
        let hb_lost = Arc::clone(&lease_lost_flags);
        let hb_cursor = Arc::clone(&batch_cursor);
        let heartbeat_handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(hb_interval);
            interval.tick().await; // first tick immediate
            loop {
                tokio::select! {
                    _ = &mut cancel_rx => break,
                    _ = interval.tick() => {
                        let cursor_guard = hb_cursor.lock().await;
                        let start_idx = *cursor_guard;
                        for idx in start_idx..hb_leases.len() {
                            if hb_lost[idx].load(Ordering::SeqCst) {
                                continue;
                            }
                            if let Some((ref msg_id, ref token)) = hb_leases[idx]
                                && let Err(e) = hb_repo
                                    .renew_outbox_lease(msg_id, token, renew_extension_secs)
                                    .await
                            {
                                tracing::error!(
                                    message_id = %msg_id,
                                    error = %e,
                                    "Failed to renew outbox lease heartbeat; lease ownership lost during batch dispatch"
                                );
                                hb_lost[idx].store(true, Ordering::SeqCst);
                            }
                        }
                    }
                }
            }
        });

        // Ensure the heartbeat task (and its Arc<Repository> reference) is aborted immediately
        // if poll_and_dispatch is dropped by an outer drain timeout.
        struct AbortOnDrop(tokio::task::AbortHandle);
        impl Drop for AbortOnDrop {
            fn drop(&mut self) {
                self.0.abort();
            }
        }
        let _hb_guard = AbortOnDrop(heartbeat_handle.abort_handle());

        for (idx, message) in pending.into_iter().enumerate() {
            let token = match message.lease_token.as_deref() {
                Some(t) => t,
                None => {
                    tracing::error!(message_id = %message.id, "Claimed outbox message is missing lease token");
                    *batch_cursor.lock().await = idx + 1;
                    result.failed_transitions += 1;
                    continue;
                }
            };

            let lease_lost = Arc::clone(&lease_lost_flags[idx]);
            if lease_lost.load(Ordering::SeqCst) {
                tracing::warn!(
                    message_id = %message.id,
                    "Outbox message lease was lost while waiting in batch; skipping dispatch"
                );
                *batch_cursor.lock().await = idx + 1;
                result.failed_transitions += 1;
                continue;
            }

            // Renew the lease before dispatching to refresh it after waiting in batch
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
                lease_lost.store(true, Ordering::SeqCst);
                *batch_cursor.lock().await = idx + 1;
                result.failed_transitions += 1;
                continue;
            }

            let cancel_signal = Arc::clone(&lease_lost);
            let fut_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                (self.dispatcher)(&message, cancel_signal)
            }));

            let tc = message
                .trace_context
                .as_deref()
                .and_then(rume_observability::TraceContext::parse_serialized)
                .map(|ctx| ctx.new_child())
                .unwrap_or_else(rume_observability::TraceContext::generate);

            let span = tracing::info_span!(
                "outbox.dispatch",
                message.id = %message.id,
                message.event_type = %message.event_type,
                traceparent = %tc.traceparent,
                tracestate = tc.tracestate.as_deref().unwrap_or(""),
            );

            let dispatch_result = match fut_result {
                Ok(fut) => {
                    let scoped_fut = tc.scope_future(span, fut);
                    match std::panic::AssertUnwindSafe(scoped_fut)
                        .catch_unwind()
                        .await
                    {
                        Ok(res) => res,
                        Err(_) => Err(OutboxError::Dispatch("outbox dispatcher panicked".into())),
                    }
                }
                Err(_) => Err(OutboxError::Dispatch("outbox dispatcher panicked".into())),
            };

            // Advance the batch cursor under `batch_cursor` so any in-flight heartbeat tick
            // finishes before we inspect `lease_lost` and transition the message state.
            {
                let mut cursor_guard = batch_cursor.lock().await;
                *cursor_guard = idx + 1;
            }

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
                Ok(()) => match self.repository.mark_published(&message.id, token).await {
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
                },
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

        let _ = cancel_tx.send(());
        let _ = heartbeat_handle.await;

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
            total_purged: self.total_purged.load(Ordering::Relaxed),
            last_error: last_err,
            is_healthy: consecutive < 5,
        }
    }

    /// Explicitly executes a published message sweep pass to prevent unbounded table bloat.
    ///
    /// Purges up to `batch_size` messages with status `published` created at or before `retention_secs` ago.
    /// Returns the count of hard-deleted rows.
    pub async fn sweep_published(
        &self,
        retention_secs: u64,
        batch_size: usize,
    ) -> Result<usize, OutboxError> {
        let count = self
            .repository
            .purge_published(retention_secs, batch_size)
            .await?;
        if count > 0 {
            self.total_purged.fetch_add(count as u64, Ordering::SeqCst);
            tracing::info!(purged = count, "Outbox sweeper purged published messages");
        }
        Ok(count)
    }

    /// Spawns a background maintenance sweeper that periodically purges old published outbox rows.
    pub fn start_sweeper(&self, config: OutboxSweeperConfig) -> tokio::task::JoinHandle<()> {
        let relay = self.clone();
        let mut shutdown_rx = self.shutdown_tx.subscribe();
        tokio::spawn(async move {
            while !*shutdown_rx.borrow() {
                if let Err(err) = relay
                    .sweep_published(config.retention_secs, config.batch_size)
                    .await
                {
                    tracing::error!(error = %err, "Outbox background sweeper encountered an error");
                }
                tokio::select! {
                    biased;
                    _ = shutdown_rx.wait_for(|&stopped| stopped) => { break; }
                    _ = sleep(config.interval) => {},
                }
            }
        })
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
        let mut shutdown_rx = self.shutdown_tx.subscribe();
        tokio::spawn(async move {
            let interval = relay.config.poll_interval;
            let batch = relay.config.batch_size;
            while !*shutdown_rx.borrow() {
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
                    biased;
                    _ = shutdown_rx.wait_for(|&stopped| stopped) => { break; }
                    _ = sleep(interval) => {},
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
    /// # Graceful Shutdown & Absolute Deadline Invariants:
    /// This method guarantees that shutdown **never hangs** beyond `timeout`, even if the underlying
    /// database repository experiences transient stalls, high connection latency, or deadlocks.
    ///
    /// An absolute deadline (`start + timeout`) is established upfront. Every asynchronous operation
    /// inside the drain loop—including checking [`OutboxRepository::pending_count`], dispatching batches
    /// via [`OutboxRelay::poll_and_dispatch`], and inter-batch sleep—is strictly bounded by the
    /// remaining duration until `deadline`. If any operation exceeds the deadline, it aborts cleanly,
    /// logs a warning, and returns `Ok(false)` rather than blocking process termination.
    ///
    /// Returns `Ok(true)` if all messages were drained, `Ok(false)` if the drain
    /// timed out, or `Err(err)` if an unrecoverable repository error occurred while
    /// checking pending count or dispatching messages.
    pub async fn try_drain(&self, timeout: Duration) -> Result<bool, OutboxError> {
        self.is_stopping.store(true, Ordering::SeqCst);
        // Why: `send_replace(true)` retains the shutdown flag even when no background loop
        // has subscribed yet, whereas `send(true)` drops the value when `receiver_count() == 0`.
        self.shutdown_tx.send_replace(true);
        let start = std::time::Instant::now();
        let deadline = start + timeout;

        loop {
            let now = std::time::Instant::now();
            if now >= deadline {
                tracing::warn!("Outbox relay drain timed out");
                return Ok(false);
            }
            let remaining = deadline.saturating_duration_since(now);

            let count = match tokio::time::timeout(remaining, self.repository.pending_count()).await
            {
                Ok(Ok(c)) => c,
                Ok(Err(err)) => return Err(err),
                Err(_) => {
                    tracing::warn!("Outbox relay drain timed out while checking pending count");
                    return Ok(false);
                }
            };

            if count == 0 {
                return Ok(true);
            }

            let now = std::time::Instant::now();
            if now >= deadline {
                tracing::warn!(
                    remaining = count,
                    "Outbox relay drain timed out with messages remaining"
                );
                return Ok(false);
            }
            let remaining = deadline.saturating_duration_since(now);

            let res = match tokio::time::timeout(
                remaining,
                self.poll_and_dispatch(self.config.batch_size),
            )
            .await
            {
                Ok(Ok(res)) => res,
                Ok(Err(err)) => return Err(err),
                Err(_) => {
                    tracing::warn!(
                        remaining = count,
                        "Outbox relay drain timed out during message dispatch"
                    );
                    return Ok(false);
                }
            };

            if !res.is_success() {
                return Err(OutboxError::Repository(format!(
                    "outbox relay encountered {} failed state transitions during drain",
                    res.failed_transitions
                )));
            }

            let now = std::time::Instant::now();
            if now >= deadline {
                return Ok(false);
            }
            let remaining = deadline.saturating_duration_since(now);
            let sleep_dur = Duration::from_millis(20).min(remaining);
            if sleep_dur.is_zero() {
                return Ok(false);
            }
            sleep(sleep_dur).await;
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
            meta.insert(
                "is_healthy".to_string(),
                serde_json::json!(status.is_healthy),
            );
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
                    duration_ms: None,
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
                    duration_ms: None,
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
                    duration_ms: None,
                }
            } else {
                HealthCheckResult {
                    name,
                    status: HealthStatus::Ok,
                    message: "outbox relay healthy".to_string(),
                    meta,
                    cached: false,
                    duration_ms: None,
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::outbox::BoxFuture;

    #[derive(Debug)]
    struct NoopOutboxRepo;

    impl OutboxRepository for NoopOutboxRepo {
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
            _max_retries: u32,
        ) -> BoxFuture<'_, Result<(), OutboxError>> {
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
    async fn try_drain_before_start_retains_shutdown_signal() {
        let repo = Arc::new(NoopOutboxRepo);
        let relay = OutboxRelay::new(repo, |_msg| async { Ok(()) });

        assert!(relay.try_drain(Duration::from_secs(1)).await.unwrap());

        let handle = relay.start_with_config();
        tokio::time::timeout(Duration::from_secs(2), handle)
            .await
            .expect("relay loop started after try_drain must exit immediately")
            .expect("relay task must not panic");

        let sweeper = relay.start_sweeper(OutboxSweeperConfig::default());
        tokio::time::timeout(Duration::from_secs(2), sweeper)
            .await
            .expect("sweeper started after try_drain must exit immediately")
            .expect("sweeper task must not panic");
    }

    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn outbox_relay_publishes_to_message_broker() {
        use crate::broker::{InMemoryBroker, MessageBroker};
        use rume_observability::TraceContext;

        let backend = Arc::new(
            rume_data::persistence::SqliteBackend::in_memory()
                .await
                .expect("sqlite in-memory"),
        );
        backend
            .init_outbox_table()
            .await
            .expect("init outbox table");

        let trace_ctx = TraceContext::generate();
        let msg = OutboxMessage::new("msg-broker-1", "order.created", br#"{"id":1}"#.to_vec())
            .with_trace_context(trace_ctx.to_string());
        backend.save(msg).await.expect("save outbox");

        let broker = Arc::new(InMemoryBroker::new());
        let mut consumer = broker.subscribe("order.created").await.expect("subscribe");

        let relay = OutboxRelay::with_broker(Arc::clone(&backend), broker);
        let poll = relay
            .poll_and_dispatch(10)
            .await
            .expect("poll_and_dispatch");
        assert_eq!(poll.published, 1);

        let delivered = consumer
            .next_message()
            .await
            .expect("next_message")
            .expect("delivered message");
        assert_eq!(delivered.topic, "order.created");
        assert_eq!(delivered.payload, br#"{"id":1}"#);
        let tp = delivered
            .headers
            .get("traceparent")
            .expect("traceparent header");
        assert!(tp.contains(trace_ctx.trace_id()));
    }

    #[tokio::test]
    async fn outbox_relay_batch_heartbeat_renews_all_remaining_messages_in_batch() {
        use std::collections::HashMap;
        use std::sync::Mutex;

        #[derive(Debug)]
        struct RenewalTrackingRepo {
            messages: Mutex<Vec<OutboxMessage>>,
            renewals: Mutex<HashMap<String, usize>>,
        }

        impl OutboxRepository for RenewalTrackingRepo {
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
                let batch = std::mem::take(&mut *self.messages.lock().unwrap());
                Box::pin(async move { Ok(batch) })
            }
            fn renew_outbox_lease(
                &self,
                id: &str,
                _lease_token: &str,
                _extension_secs: u64,
            ) -> BoxFuture<'_, Result<(), OutboxError>> {
                let id = id.to_string();
                Box::pin(async move {
                    *self.renewals.lock().unwrap().entry(id).or_insert(0) += 1;
                    Ok(())
                })
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
                _max_retries: u32,
            ) -> BoxFuture<'_, Result<(), OutboxError>> {
                Box::pin(async { Ok(()) })
            }
            fn recover_expired_claims(
                &self,
                _max_lease_age_secs: u64,
            ) -> BoxFuture<'_, Result<usize, OutboxError>> {
                Box::pin(async { Ok(0) })
            }
            fn pending_count(&self) -> BoxFuture<'_, Result<usize, OutboxError>> {
                Box::pin(async { Ok(0) })
            }
            fn published_count(&self) -> BoxFuture<'_, Result<usize, OutboxError>> {
                Box::pin(async { Ok(0) })
            }
        }

        let make_msg = |id: &str| {
            let mut m = OutboxMessage::new(id, "evt.slow", b"{}".to_vec());
            m.lease_token = Some(format!("tok-{id}"));
            m
        };
        let repo = Arc::new(RenewalTrackingRepo {
            messages: Mutex::new(vec![make_msg("m1"), make_msg("m2"), make_msg("m3")]),
            renewals: Mutex::new(HashMap::new()),
        });

        let repo_for_check = Arc::clone(&repo);
        let seen_queued_renewals = Arc::new(AtomicBool::new(false));
        let seen_flag = Arc::clone(&seen_queued_renewals);
        let relay = OutboxRelay::new(Arc::clone(&repo), move |msg| {
            let id = msg.id.clone();
            let repo = Arc::clone(&repo_for_check);
            let seen_flag = Arc::clone(&seen_flag);
            async move {
                if id == "m1" {
                    // Sleep past multiple 50ms heartbeat ticks while m2 and m3 wait in batch.
                    tokio::time::sleep(Duration::from_millis(160)).await;
                    let renewals = repo.renewals.lock().unwrap();
                    if renewals.get("m2").copied().unwrap_or(0) >= 1
                        && renewals.get("m3").copied().unwrap_or(0) >= 1
                    {
                        seen_flag.store(true, Ordering::SeqCst);
                    }
                }
                Ok(())
            }
        })
        .with_config(OutboxRelayConfig {
            batch_size: 10,
            lease_duration_secs: 0, // clamps hb_interval to 50ms
            poll_interval: Duration::from_millis(50),
            max_retries: 3,
        });

        let res = relay
            .poll_and_dispatch(10)
            .await
            .expect("poll_and_dispatch");
        assert_eq!(res.published, 3);
        assert_eq!(res.failed_transitions, 0);
        assert!(
            seen_queued_renewals.load(Ordering::SeqCst),
            "remaining batch messages m2 and m3 must be heartbeated while m1 is dispatching"
        );
    }
}
