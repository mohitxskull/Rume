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
        let msg_id = msg.id.clone();
        let event_type = msg.event_type.clone();
        let payload = msg.payload.clone();
        let msg_trace_context = msg.trace_context.clone();
        Box::pin(async move {
            let mut headers = BTreeMap::new();
            headers.insert("message_id".to_string(), msg_id);
            headers.insert("event_type".to_string(), event_type.clone());
            // Why: capture `TraceContext::current()` inside the async future so it
            // observes the child span entered by `tc.scope_future(span, fut)` in
            // `OutboxRelay::poll_and_dispatch` instead of synchronously capturing
            // any ambient span active before entering the scope.
            let trace_ctx = rume_observability::TraceContext::current().or_else(|| {
                msg_trace_context
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
                topic: event_type,
                payload,
                headers,
            };
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
    /// Optional per-message dispatch timeout (default: `Some(30s)`).
    pub dispatch_timeout: Option<Duration>,
}

impl Default for OutboxRelayConfig {
    fn default() -> Self {
        Self {
            batch_size: 50,
            lease_duration_secs: 30,
            poll_interval: Duration::from_millis(500),
            max_retries: 5,
            dispatch_timeout: Some(Duration::from_secs(30)),
        }
    }
}

impl OutboxRelayConfig {
    /// Sets a custom retry limit before a message is marked as failed.
    pub fn with_max_retries(mut self, max_retries: u32) -> Self {
        self.max_retries = max_retries;
        self
    }

    /// Sets an optional per-message dispatch timeout.
    pub fn with_dispatch_timeout(mut self, timeout: Option<Duration>) -> Self {
        self.dispatch_timeout = timeout;
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
        // Tracks per-index finalization so an in-flight heartbeat renewal RPC that returns
        // an error after the dispatcher has already finished and begun/completed
        // `mark_published`/`mark_failed` never logs a spurious lease-loss error or trips
        // `hb_lost[idx]` (`RC-12`).
        let finalized_flags: Arc<Vec<AtomicBool>> =
            Arc::new((0..pending.len()).map(|_| AtomicBool::new(false)).collect());
        // Tracks the first index in `pending` not yet processed so subsequent heartbeat ticks
        // skip already-completed prefixes; does not serialize in-flight renewal RPCs (see
        // `finalized_flags` above).
        let batch_cursor = Arc::new(tokio::sync::Mutex::new(0usize));
        let (cancel_tx, mut cancel_rx) = tokio::sync::oneshot::channel::<()>();

        let hb_repo = Arc::clone(&self.repository);
        let hb_leases = Arc::clone(&batch_leases);
        let hb_lost = Arc::clone(&lease_lost_flags);
        let hb_finalized = Arc::clone(&finalized_flags);
        let hb_cursor = Arc::clone(&batch_cursor);
        let renew_timeout = hb_interval.max(Duration::from_secs(1));
        let heartbeat_handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(hb_interval);
            interval.tick().await; // first tick immediate
            loop {
                tokio::select! {
                    _ = &mut cancel_rx => break,
                    _ = interval.tick() => {
                        // Why: snapshot `batch_cursor` and release the lock before awaiting
                        // `renew_outbox_lease` so `N` sequential DB renewals never block message
                        // finalization from advancing `batch_cursor` (`P2-13`).
                        let start_idx = *hb_cursor.lock().await;
                        for idx in start_idx..hb_leases.len() {
                            if hb_finalized[idx].load(Ordering::Acquire)
                                || idx < *hb_cursor.lock().await
                                || hb_lost[idx].load(Ordering::SeqCst)
                            {
                                continue;
                            }
                            if let Some((ref msg_id, ref token)) = hb_leases[idx] {
                                // Why: bound each renewal RPC so a stalled DB cannot hang the
                                // heartbeat task indefinitely (`RC-12`).
                                match tokio::time::timeout(
                                    renew_timeout,
                                    hb_repo.renew_outbox_lease(msg_id, token, renew_extension_secs),
                                )
                                .await
                                {
                                    Ok(Ok(())) => {}
                                    Ok(Err(e)) => {
                                        if !hb_finalized[idx].load(Ordering::Acquire) {
                                            tracing::error!(
                                                message_id = %msg_id,
                                                error = %e,
                                                "Failed to renew outbox lease heartbeat; lease ownership lost during batch dispatch"
                                            );
                                            hb_lost[idx].store(true, Ordering::SeqCst);
                                        }
                                    }
                                    Err(_) => {
                                        if !hb_finalized[idx].load(Ordering::Acquire) {
                                            tracing::error!(
                                                message_id = %msg_id,
                                                timeout = ?renew_timeout,
                                                "Timed out renewing outbox lease heartbeat; lease ownership lost during batch dispatch"
                                            );
                                            hb_lost[idx].store(true, Ordering::SeqCst);
                                        }
                                    }
                                }
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
                    finalized_flags[idx].store(true, Ordering::Release);
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
                finalized_flags[idx].store(true, Ordering::Release);
                *batch_cursor.lock().await = idx + 1;
                result.failed_transitions += 1;
                continue;
            }

            // Renew the lease before dispatching to refresh it after waiting in batch.
            // Why: bound pre-dispatch lease renewal with `renew_timeout` so a stalled DB
            // cannot wedge `poll_and_dispatch` indefinitely (`P2-13`).
            match tokio::time::timeout(
                renew_timeout,
                self.repository
                    .renew_outbox_lease(&message.id, token, renew_extension_secs),
            )
            .await
            {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    tracing::warn!(
                        message_id = %message.id,
                        error = %e,
                        "Failed to renew outbox message lease before dispatch; lease expired or token mismatch"
                    );
                    lease_lost.store(true, Ordering::SeqCst);
                    finalized_flags[idx].store(true, Ordering::Release);
                    *batch_cursor.lock().await = idx + 1;
                    result.failed_transitions += 1;
                    continue;
                }
                Err(_) => {
                    tracing::warn!(
                        message_id = %message.id,
                        timeout = ?renew_timeout,
                        "Timed out renewing outbox message lease before dispatch"
                    );
                    lease_lost.store(true, Ordering::SeqCst);
                    finalized_flags[idx].store(true, Ordering::Release);
                    *batch_cursor.lock().await = idx + 1;
                    result.failed_transitions += 1;
                    continue;
                }
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
                    let guarded = std::panic::AssertUnwindSafe(scoped_fut).catch_unwind();
                    match self.config.dispatch_timeout {
                        Some(limit) => match tokio::time::timeout(limit, guarded).await {
                            Ok(Ok(res)) => res,
                            Ok(Err(_)) => {
                                Err(OutboxError::Dispatch("outbox dispatcher panicked".into()))
                            }
                            Err(_) => Err(OutboxError::Dispatch(format!(
                                "outbox dispatcher timed out after {limit:?}"
                            ))),
                        },
                        None => match guarded.await {
                            Ok(res) => res,
                            Err(_) => {
                                Err(OutboxError::Dispatch("outbox dispatcher panicked".into()))
                            }
                        },
                    }
                }
                Err(_) => Err(OutboxError::Dispatch("outbox dispatcher panicked".into())),
            };

            // Mark this index finalized before advancing `batch_cursor` and calling
            // `mark_published`/`mark_failed`, so any already-in-flight heartbeat renewal RPC
            // that observes the cleared lease token does not spuriously trip `lease_lost` (`RC-12`).
            finalized_flags[idx].store(true, Ordering::Release);
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
        // Why: use `tokio::time::Instant` so `try_drain` respects paused virtual time in tests
        // and aligns with `tokio::time::timeout` / `tokio::time::sleep` (`RC-35`).
        let start = tokio::time::Instant::now();
        let deadline = start + timeout;
        let mut idle_backoff = Duration::from_millis(20);

        loop {
            let now = tokio::time::Instant::now();
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

            let now = tokio::time::Instant::now();
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

            if res.total_processed() > 0 {
                // Progress was made; reset backoff and immediately check/drain the next batch.
                idle_backoff = Duration::from_millis(20);
                continue;
            }

            // Why: `pending_count > 0` but `poll_and_dispatch` claimed 0 rows (only
            // future-retry or peer-inflight rows remain). Use bounded exponential backoff
            // instead of 20ms busy-spinning (`RC-35`).
            let now = tokio::time::Instant::now();
            if now >= deadline {
                return Ok(false);
            }
            let remaining = deadline.saturating_duration_since(now);
            let sleep_dur = idle_backoff.min(remaining);
            if sleep_dur.is_zero() {
                return Ok(false);
            }
            sleep(sleep_dur).await;
            idle_backoff = (idle_backoff * 2).min(Duration::from_millis(500));
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
            ..Default::default()
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

    #[tokio::test]
    async fn broker_dispatcher_captures_scoped_child_span_not_ambient_sync_span() {
        use crate::broker::{InMemoryBroker, MessageBroker};
        use rume_observability::TraceContext;

        let broker = Arc::new(InMemoryBroker::new());
        let mut consumer = broker.subscribe("order.scoped").await.expect("subscribe");
        let dispatcher = broker_dispatcher(broker);

        let ambient_tc = TraceContext::generate();
        let child_tc = TraceContext::generate();
        let msg = OutboxMessage::new("msg-scoped-1", "order.scoped", b"{}".to_vec());

        // Invoke the dispatcher closure while an unrelated ambient OTel context is attached
        // synchronously, then await the returned future inside `child_tc.scope_future(...)`.
        let fut = {
            let _ambient_guard = ambient_tc.to_otel_context().attach();
            dispatcher(&msg)
        };

        let span = tracing::info_span!("outbox.dispatch.test");
        child_tc
            .scope_future(span, fut)
            .await
            .expect("broker dispatch");

        let delivered = consumer
            .next_message()
            .await
            .expect("next_message")
            .expect("delivered");
        let tp = delivered
            .headers
            .get("traceparent")
            .expect("traceparent header");
        assert_eq!(
            tp, &child_tc.traceparent,
            "broker_dispatcher must capture the async scoped span, not the synchronous ambient span"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn outbox_relay_enforces_dispatch_timeout_and_try_drain_uses_tokio_time() {
        use std::sync::Mutex;

        #[derive(Debug)]
        struct TimeoutAndIdleRepo {
            messages: Mutex<Vec<OutboxMessage>>,
            pending_always: AtomicU32,
            poll_calls: AtomicU32,
        }

        impl OutboxRepository for TimeoutAndIdleRepo {
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
                self.poll_calls.fetch_add(1, Ordering::SeqCst);
                let batch = std::mem::take(&mut *self.messages.lock().unwrap());
                Box::pin(async move { Ok(batch) })
            }
            fn renew_outbox_lease(
                &self,
                _id: &str,
                _lease_token: &str,
                _extension_secs: u64,
            ) -> BoxFuture<'_, Result<(), OutboxError>> {
                Box::pin(async { Ok(()) })
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
                let c = self.pending_always.load(Ordering::SeqCst) as usize;
                Box::pin(async move { Ok(c) })
            }
            fn published_count(&self) -> BoxFuture<'_, Result<usize, OutboxError>> {
                Box::pin(async { Ok(0) })
            }
        }

        let mut msg = OutboxMessage::new("m-hang", "evt.hang", b"{}".to_vec());
        msg.lease_token = Some("tok-hang".into());
        let repo = Arc::new(TimeoutAndIdleRepo {
            messages: Mutex::new(vec![msg]),
            pending_always: AtomicU32::new(1),
            poll_calls: AtomicU32::new(0),
        });

        let relay = OutboxRelay::new(Arc::clone(&repo), |_msg| async {
            std::future::pending::<Result<(), OutboxError>>().await
        })
        .with_config(OutboxRelayConfig {
            dispatch_timeout: Some(Duration::from_millis(100)),
            ..Default::default()
        });

        // Hanging dispatcher must time out and schedule a retry instead of blocking forever.
        let poll = relay.poll_and_dispatch(10).await.expect("poll");
        assert_eq!(poll.published, 0);
        assert_eq!(poll.retried, 1);

        // With 1 unclaimable future-retry row remaining, `try_drain(2s)` must finish via
        // paused tokio time with exponential backoff (far fewer polls than 20ms spinning = 100 polls).
        repo.poll_calls.store(0, Ordering::SeqCst);
        let drained = relay
            .try_drain(Duration::from_secs(2))
            .await
            .expect("drain");
        assert!(!drained);
        let calls = repo.poll_calls.load(Ordering::SeqCst);
        assert!(
            calls < 15,
            "try_drain should back off when no rows are claimable instead of 20ms spinning (got {calls} polls)"
        );
    }

    #[tokio::test]
    async fn outbox_relay_bounds_pre_dispatch_lease_renewal_with_timeout() {
        tokio::time::pause();

        #[derive(Debug)]
        struct HangingRenewRepo {
            messages: std::sync::Mutex<Vec<OutboxMessage>>,
        }

        impl OutboxRepository for HangingRenewRepo {
            fn save(&self, _message: OutboxMessage) -> BoxFuture<'_, Result<(), OutboxError>> {
                Box::pin(async { Ok(()) })
            }
            fn fetch_pending(
                &self,
                _limit: usize,
            ) -> BoxFuture<'_, Result<Vec<OutboxMessage>, OutboxError>> {
                Box::pin(async { Ok(Vec::new()) })
            }
            fn claim_pending_with_lease(
                &self,
                _limit: usize,
                _lease_duration_secs: u64,
            ) -> BoxFuture<'_, Result<Vec<OutboxMessage>, OutboxError>> {
                let msgs = std::mem::take(&mut *self.messages.lock().unwrap());
                Box::pin(async move { Ok(msgs) })
            }
            fn renew_outbox_lease(
                &self,
                _id: &str,
                _lease_token: &str,
                _extension_secs: u64,
            ) -> BoxFuture<'_, Result<(), OutboxError>> {
                // Hang forever to verify pre-dispatch renewal is bounded by `renew_timeout` (`P2-13`).
                Box::pin(async {
                    std::future::pending::<()>().await;
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

        let mut msg = OutboxMessage::new("m-renew-hang", "evt.hang", b"{}".to_vec());
        msg.lease_token = Some("tok-1".into());
        let repo = Arc::new(HangingRenewRepo {
            messages: std::sync::Mutex::new(vec![msg]),
        });
        let dispatched = Arc::new(AtomicU32::new(0));
        let dispatched_clone = Arc::clone(&dispatched);

        let relay = OutboxRelay::new(repo, move |_msg| {
            let d = Arc::clone(&dispatched_clone);
            async move {
                d.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        });

        let poll = relay
            .poll_and_dispatch(10)
            .await
            .expect("poll must not hang");
        assert_eq!(poll.published, 0);
        assert_eq!(poll.failed_transitions, 1);
        assert_eq!(dispatched.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn finalized_flag_suppresses_spurious_lease_loss_from_late_heartbeat_error() {
        use tokio::sync::Notify;

        #[derive(Debug)]
        struct LateHeartbeatErrorRepo {
            messages: std::sync::Mutex<Vec<OutboxMessage>>,
            renew_calls: AtomicU32,
            hb_started: Arc<Notify>,
            hb_release: Arc<Notify>,
        }

        impl OutboxRepository for LateHeartbeatErrorRepo {
            fn save(&self, _message: OutboxMessage) -> BoxFuture<'_, Result<(), OutboxError>> {
                Box::pin(async { Ok(()) })
            }
            fn fetch_pending(
                &self,
                _limit: usize,
            ) -> BoxFuture<'_, Result<Vec<OutboxMessage>, OutboxError>> {
                Box::pin(async { Ok(Vec::new()) })
            }
            fn claim_pending_with_lease(
                &self,
                _batch_size: usize,
                _lease_duration_secs: u64,
            ) -> BoxFuture<'_, Result<Vec<OutboxMessage>, OutboxError>> {
                let msgs = std::mem::take(&mut *self.messages.lock().unwrap());
                Box::pin(async move { Ok(msgs) })
            }
            fn renew_outbox_lease(
                &self,
                _message_id: &str,
                _lease_token: &str,
                _lease_duration_secs: u64,
            ) -> BoxFuture<'_, Result<(), OutboxError>> {
                let call = self.renew_calls.fetch_add(1, Ordering::SeqCst);
                let started = Arc::clone(&self.hb_started);
                let release = Arc::clone(&self.hb_release);
                Box::pin(async move {
                    if call == 0 {
                        // Pre-dispatch renewal succeeds.
                        Ok(())
                    } else {
                        // In-flight heartbeat tick blocks until after finalization, then fails
                        // as if `mark_published` already cleared the lease token.
                        started.notify_one();
                        release.notified().await;
                        Err(OutboxError::Repository("lease already cleared".into()))
                    }
                })
            }
            fn mark_published(
                &self,
                _message_id: &str,
                _lease_token: &str,
            ) -> BoxFuture<'_, Result<(), OutboxError>> {
                self.hb_release.notify_one();
                Box::pin(async { Ok(()) })
            }
            fn mark_failed(
                &self,
                _message_id: &str,
                _lease_token: &str,
                _error: String,
            ) -> BoxFuture<'_, Result<(), OutboxError>> {
                Box::pin(async { Ok(()) })
            }
            fn mark_failed_with_max_retries(
                &self,
                _message_id: &str,
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

        let hb_started = Arc::new(Notify::new());
        let hb_release = Arc::new(Notify::new());
        let mut msg = OutboxMessage::new("m-late-hb", "evt.late", b"{}".to_vec());
        msg.lease_token = Some("tok-1".into());
        let repo = Arc::new(LateHeartbeatErrorRepo {
            messages: std::sync::Mutex::new(vec![msg]),
            renew_calls: AtomicU32::new(0),
            hb_started: Arc::clone(&hb_started),
            hb_release: Arc::clone(&hb_release),
        });

        let hb_started_disp = Arc::clone(&hb_started);
        let relay = OutboxRelay::new(repo, move |_msg| {
            let started = Arc::clone(&hb_started_disp);
            async move {
                // Wait until the background heartbeat tick is in-flight before completing dispatch.
                let _ = tokio::time::timeout(Duration::from_secs(2), started.notified()).await;
                Ok(())
            }
        })
        .with_config(OutboxRelayConfig {
            lease_duration_secs: 1,
            ..Default::default()
        });

        let poll = relay
            .poll_and_dispatch(10)
            .await
            .expect("poll must succeed");
        assert_eq!(poll.published, 1);
        assert_eq!(poll.failed_transitions, 0);
    }
}
