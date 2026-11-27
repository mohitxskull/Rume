//! `RealtimeHub`, configuration, room channels, and `PubSub` backplane.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use parking_lot::{Mutex, RwLock};
use rume_foundation::error::AppError;
use rume_messaging::pubsub::PubSub;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::{Notify, broadcast, watch};

/// Configuration for [`RealtimeHub`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RealtimeConfig {
    /// Maximum inbound WebSocket message size in bytes.
    pub max_message_size: usize,
    /// Per-room local broadcast channel capacity.
    pub channel_capacity: usize,
    /// Interval between server-initiated WebSocket Ping frames / SSE keep-alives.
    pub heartbeat_interval: Duration,
    /// Maximum time to wait for a WebSocket Pong before closing the client.
    pub heartbeat_timeout: Duration,
    /// Maximum time `RealtimeHub::shutdown` waits for active connections to drain.
    pub shutdown_drain_timeout: Duration,
}

impl Default for RealtimeConfig {
    fn default() -> Self {
        Self {
            max_message_size: 64 * 1024,
            channel_capacity: 256,
            heartbeat_interval: Duration::from_secs(30),
            heartbeat_timeout: Duration::from_secs(10),
            shutdown_drain_timeout: Duration::from_secs(5),
        }
    }
}

impl RealtimeConfig {
    /// Sets the maximum inbound WebSocket message size in bytes.
    pub fn with_max_message_size(mut self, bytes: usize) -> Self {
        self.max_message_size = bytes.max(1);
        self
    }

    /// Sets the per-room local broadcast channel capacity.
    pub fn with_channel_capacity(mut self, capacity: usize) -> Self {
        self.channel_capacity = capacity.max(1);
        self
    }

    /// Sets the heartbeat ping / keep-alive interval.
    pub fn with_heartbeat_interval(mut self, interval: Duration) -> Self {
        self.heartbeat_interval = interval;
        self
    }

    /// Sets the WebSocket Pong timeout.
    pub fn with_heartbeat_timeout(mut self, timeout: Duration) -> Self {
        self.heartbeat_timeout = timeout;
        self
    }

    /// Sets the graceful shutdown drain timeout.
    pub fn with_shutdown_drain_timeout(mut self, timeout: Duration) -> Self {
        self.shutdown_drain_timeout = timeout;
        self
    }
}

/// Event broadcast over [`RealtimeHub`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RealtimeEvent {
    /// Event name (mapped to SSE `event:` and JSON `"event"`).
    pub event: String,
    /// JSON payload.
    pub data: serde_json::Value,
    /// Optional event ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

impl RealtimeEvent {
    /// Creates a new [`RealtimeEvent`] with the given event name and JSON payload.
    pub fn new(event: impl Into<String>, data: serde_json::Value) -> Self {
        Self {
            event: event.into(),
            data,
            id: None,
        }
    }

    /// Attaches an event ID.
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = Some(id.into());
        self
    }
}

/// Errors returned by [`RealtimeHub`], auth extractors, and handlers.
#[derive(Debug, Error)]
pub enum RealtimeError {
    /// Invalid tenant or room identifier (e.g. empty or containing `:`).
    #[error("invalid {kind} identifier: {value:?}")]
    InvalidIdentifier { kind: &'static str, value: String },
    /// Backplane pub/sub error.
    #[error("realtime pubsub error: {0}")]
    PubSub(String),
    /// Hub is shutting down.
    #[error("realtime hub is shutting down")]
    ShuttingDown,
    /// Missing or invalid authentication credentials.
    #[error("unauthorized: {0}")]
    Unauthorized(String),
    /// Authenticated subject is not permitted to access the requested tenant or room.
    #[error("forbidden: {0}")]
    Forbidden(String),
}

impl IntoResponse for RealtimeError {
    fn into_response(self) -> Response {
        match self {
            Self::InvalidIdentifier { .. } => {
                (StatusCode::BAD_REQUEST, self.to_string()).into_response()
            }
            Self::PubSub(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg).into_response(),
            Self::ShuttingDown => {
                (StatusCode::SERVICE_UNAVAILABLE, self.to_string()).into_response()
            }
            Self::Unauthorized(_) => AppError::Unauthorized.into_response(),
            Self::Forbidden(_) => AppError::Forbidden.into_response(),
        }
    }
}

/// Result of receiving from a [`RealtimeSubscription`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RealtimeRecv {
    /// A broadcast event.
    Event(RealtimeEvent),
    /// The subscriber lagged behind by `u64` messages.
    Lagged(u64),
    /// The hub is shutting down gracefully.
    Shutdown,
    /// The underlying broadcast channel closed.
    Closed,
}

#[derive(Debug, Serialize, Deserialize)]
struct PubSubEnvelope {
    origin_node_id: String,
    event: RealtimeEvent,
}

/// Subscription handle to a tenant-scoped room on [`RealtimeHub`].
#[derive(Debug)]
pub struct RealtimeSubscription {
    rx: Option<broadcast::Receiver<RealtimeEvent>>,
    shutdown_rx: watch::Receiver<bool>,
    hub: Arc<RealtimeHubInner>,
    key: (String, String),
}

impl RealtimeSubscription {
    /// Receives the next event, lag notification, or shutdown signal.
    pub async fn recv(&mut self) -> RealtimeRecv {
        if *self.shutdown_rx.borrow() {
            return RealtimeRecv::Shutdown;
        }
        let Some(rx) = self.rx.as_mut() else {
            return RealtimeRecv::Closed;
        };
        tokio::select! {
            biased;
            res = self.shutdown_rx.changed() => {
                if res.is_ok() && *self.shutdown_rx.borrow() {
                    RealtimeRecv::Shutdown
                } else {
                    RealtimeRecv::Closed
                }
            }
            msg = rx.recv() => match msg {
                Ok(ev) => RealtimeRecv::Event(ev),
                Err(broadcast::error::RecvError::Lagged(skipped)) => RealtimeRecv::Lagged(skipped),
                Err(broadcast::error::RecvError::Closed) => RealtimeRecv::Closed,
            }
        }
    }
}

impl Drop for RealtimeSubscription {
    fn drop(&mut self) {
        // Drop our receiver before inspecting `receiver_count()` so it is no longer counted.
        drop(self.rx.take());
        // Why: Evict empty room channels and abort their pubsub pump tasks when the last
        // local subscriber disconnects so ephemeral rooms never leak memory or broker subscriptions.
        let mut channels = self.hub.channels.write();
        let should_evict = channels
            .get(&self.key)
            .is_some_and(|tx| tx.receiver_count() == 0);
        if should_evict {
            channels.remove(&self.key);
            if let Some(handle) = self.hub.pubsub_tasks.lock().remove(&self.key) {
                handle.abort();
            }
        }
    }
}

/// RAII guard tracking an active WebSocket or SSE connection for graceful shutdown drain.
#[derive(Debug)]
pub(crate) struct ConnectionGuard {
    inner: Arc<RealtimeHubInner>,
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        if self.inner.active_connections.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.inner.drain_notify.notify_waiters();
        }
    }
}

#[derive(Debug)]
pub(crate) struct RealtimeHubInner {
    config: RealtimeConfig,
    node_id: String,
    channels: RwLock<HashMap<(String, String), broadcast::Sender<RealtimeEvent>>>,
    pubsub: Option<Arc<dyn PubSub>>,
    pubsub_tasks: Mutex<HashMap<(String, String), tokio::task::JoinHandle<()>>>,
    shutdown_tx: watch::Sender<bool>,
    active_connections: AtomicUsize,
    drain_notify: Notify,
}

/// Multi-tenant real-time hub supporting WebSockets, SSE, and optional `PubSub` backplane.
#[derive(Debug, Clone)]
pub struct RealtimeHub {
    inner: Arc<RealtimeHubInner>,
}

impl Default for RealtimeHub {
    fn default() -> Self {
        Self::new(RealtimeConfig::default())
    }
}

impl RealtimeHub {
    /// Creates a new [`RealtimeHub`] with the given configuration.
    pub fn new(config: RealtimeConfig) -> Self {
        let (shutdown_tx, _) = watch::channel(false);
        Self {
            inner: Arc::new(RealtimeHubInner {
                config,
                node_id: uuid::Uuid::new_v4().to_string(),
                channels: RwLock::new(HashMap::new()),
                pubsub: None,
                pubsub_tasks: Mutex::new(HashMap::new()),
                shutdown_tx,
                active_connections: AtomicUsize::new(0),
                drain_notify: Notify::new(),
            }),
        }
    }

    /// Attaches a distributed [`PubSub`] backplane for multi-instance fan-out.
    pub fn with_pubsub(self, pubsub: Arc<dyn PubSub>) -> Self {
        let (shutdown_tx, _) = watch::channel(false);
        Self {
            inner: Arc::new(RealtimeHubInner {
                config: self.inner.config.clone(),
                node_id: self.inner.node_id.clone(),
                channels: RwLock::new(HashMap::new()),
                pubsub: Some(pubsub),
                pubsub_tasks: Mutex::new(HashMap::new()),
                shutdown_tx,
                active_connections: AtomicUsize::new(0),
                drain_notify: Notify::new(),
            }),
        }
    }

    /// Returns this hub instance's unique `origin_node_id`.
    pub fn node_id(&self) -> &str {
        &self.inner.node_id
    }

    /// Returns the hub configuration.
    pub fn config(&self) -> &RealtimeConfig {
        &self.inner.config
    }

    /// Returns `true` if [`Self::shutdown`] has been triggered.
    pub fn is_shutting_down(&self) -> bool {
        *self.inner.shutdown_tx.borrow()
    }

    pub(crate) fn track_connection(&self) -> ConnectionGuard {
        self.inner.active_connections.fetch_add(1, Ordering::AcqRel);
        ConnectionGuard {
            inner: Arc::clone(&self.inner),
        }
    }

    pub(crate) fn validate_segment(kind: &'static str, value: &str) -> Result<(), RealtimeError> {
        if value.is_empty() || value.contains(':') || value.chars().any(char::is_control) {
            return Err(RealtimeError::InvalidIdentifier {
                kind,
                value: value.to_string(),
            });
        }
        Ok(())
    }

    fn topic_for(tenant_id: &str, room: &str) -> String {
        format!("realtime:{tenant_id}:{room}")
    }

    async fn ensure_pubsub_pump(
        &self,
        tenant_id: &str,
        room: &str,
        tx: broadcast::Sender<RealtimeEvent>,
    ) -> Result<(), RealtimeError> {
        let Some(ref pubsub) = self.inner.pubsub else {
            return Ok(());
        };
        let key = (tenant_id.to_string(), room.to_string());
        if self.inner.pubsub_tasks.lock().contains_key(&key) {
            return Ok(());
        }

        let topic = Self::topic_for(tenant_id, room);
        let mut sub = pubsub
            .subscribe(&topic)
            .await
            .map_err(|e| RealtimeError::PubSub(e.to_string()))?;

        if self.is_shutting_down() {
            return Err(RealtimeError::ShuttingDown);
        }

        let node_id = self.inner.node_id.clone();
        let mut shutdown_rx = self.inner.shutdown_tx.subscribe();
        let handle = tokio::spawn(async move {
            // Why: `watch::Sender::subscribe()` marks the current value as seen, so if shutdown
            // was already signaled before `subscribe()`, `changed()` would never wake.
            if *shutdown_rx.borrow() {
                return;
            }
            loop {
                let msg = tokio::select! {
                    biased;
                    res = shutdown_rx.changed() => {
                        if res.is_err() || *shutdown_rx.borrow() {
                            break;
                        }
                        continue;
                    }
                    msg = sub.recv() => msg,
                };
                match msg {
                    Ok(Some(ps_msg)) => {
                        match serde_json::from_slice::<PubSubEnvelope>(&ps_msg.payload) {
                            Ok(env) => {
                                // Why: ignore messages published by this node to prevent echo loops
                                // since local subscribers already received them directly via `tx.send`.
                                if env.origin_node_id != node_id {
                                    let _ = tx.send(env.event);
                                }
                            }
                            Err(err) => {
                                tracing::warn!(error = %err, topic = %ps_msg.topic, "invalid pubsub envelope");
                            }
                        }
                    }
                    Ok(None) => break,
                    Err(err) => {
                        tracing::warn!(error = %err, "realtime pubsub subscription error");
                        break;
                    }
                }
            }
        });

        let mut tasks = self.inner.pubsub_tasks.lock();
        // Why: `initiate_shutdown` sets `shutdown_tx` to `true` before locking `pubsub_tasks`.
        // Re-checking `is_shutting_down()` while holding `pubsub_tasks` closes the race window
        // so a late pump handle can never be inserted as a zombie task after shutdown drains.
        if self.is_shutting_down() {
            handle.abort();
            return Err(RealtimeError::ShuttingDown);
        }
        if let std::collections::hash_map::Entry::Vacant(entry) = tasks.entry(key) {
            entry.insert(handle);
        } else {
            handle.abort();
        }
        Ok(())
    }

    /// Publishes an event to `(tenant_id, room)`.
    pub async fn publish(
        &self,
        tenant_id: &str,
        room: &str,
        event: RealtimeEvent,
    ) -> Result<usize, RealtimeError> {
        Self::validate_segment("tenant_id", tenant_id)?;
        Self::validate_segment("room", room)?;
        if self.is_shutting_down() {
            return Err(RealtimeError::ShuttingDown);
        }

        // Why: Look up existing local room channel without inserting a new entry so
        // publishing to rooms with no local subscribers never leaks channel map entries.
        let local_receivers = {
            let key = (tenant_id.to_string(), room.to_string());
            self.inner
                .channels
                .read()
                .get(&key)
                .and_then(|tx| tx.send(event.clone()).ok())
                .unwrap_or(0)
        };

        if let Some(ref pubsub) = self.inner.pubsub {
            let topic = Self::topic_for(tenant_id, room);
            let envelope = PubSubEnvelope {
                origin_node_id: self.inner.node_id.clone(),
                event,
            };
            let payload = serde_json::to_vec(&envelope)
                .map_err(|e| RealtimeError::PubSub(format!("serialize envelope: {e}")))?;
            pubsub
                .publish(&topic, &payload)
                .await
                .map_err(|e| RealtimeError::PubSub(e.to_string()))?;
        }

        Ok(local_receivers)
    }

    /// Subscribes to `(tenant_id, room)`.
    pub async fn subscribe(
        &self,
        tenant_id: &str,
        room: &str,
    ) -> Result<RealtimeSubscription, RealtimeError> {
        Self::validate_segment("tenant_id", tenant_id)?;
        Self::validate_segment("room", room)?;
        if self.is_shutting_down() {
            return Err(RealtimeError::ShuttingDown);
        }

        let key = (tenant_id.to_string(), room.to_string());
        // Why: Subscribe while holding the write lock and wrap `rx` in `RealtimeSubscription`
        // before awaiting `ensure_pubsub_pump` so concurrent drops cannot evict the room mid-setup
        // and any pump error automatically cleans up via `Drop`.
        let (tx, rx) = {
            let mut guard = self.inner.channels.write();
            let tx = guard
                .entry(key.clone())
                .or_insert_with(|| broadcast::channel(self.inner.config.channel_capacity).0)
                .clone();
            let rx = tx.subscribe();
            (tx, rx)
        };
        let sub = RealtimeSubscription {
            rx: Some(rx),
            shutdown_rx: self.inner.shutdown_tx.subscribe(),
            hub: Arc::clone(&self.inner),
            key,
        };
        self.ensure_pubsub_pump(tenant_id, room, tx).await?;

        Ok(sub)
    }

    /// Broadcasts the shutdown signal to all active WebSocket and SSE streams and
    /// aborts background pub/sub pump tasks without waiting for connections to drain.
    pub fn initiate_shutdown(&self) {
        self.inner.shutdown_tx.send_replace(true);
        let handles: Vec<_> = {
            let mut tasks = self.inner.pubsub_tasks.lock();
            tasks.drain().map(|(_, h)| h).collect()
        };
        for handle in handles {
            handle.abort();
        }
    }

    /// Waits up to `shutdown_drain_timeout` for all active WebSocket and SSE
    /// connections to close.
    pub async fn wait_drained(&self) {
        if self.inner.active_connections.load(Ordering::Acquire) == 0 {
            return;
        }
        let deadline = tokio::time::Instant::now() + self.inner.config.shutdown_drain_timeout;
        loop {
            // Why: Register and `.enable()` the waiter before checking `active_connections`
            // so a `ConnectionGuard::drop` calling `notify_waiters()` between the load and
            // the await cannot cause a lost wakeup that stalls until `deadline`.
            let notified = self.inner.drain_notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.inner.active_connections.load(Ordering::Acquire) == 0 {
                return;
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                tracing::warn!(
                    remaining = self.inner.active_connections.load(Ordering::Acquire),
                    "realtime shutdown drain timeout elapsed"
                );
                break;
            }
        }
    }

    /// Signals graceful shutdown to all active WebSocket and SSE streams and waits
    /// up to `shutdown_drain_timeout` for them to close.
    pub async fn shutdown(&self) {
        self.initiate_shutdown();
        self.wait_drained().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rume_messaging::pubsub::InMemoryPubSub;

    #[tokio::test]
    async fn realtime_hub_evicts_empty_rooms_and_pubsub_tasks_on_drop() {
        let pubsub = Arc::new(InMemoryPubSub::new());
        let hub = RealtimeHub::new(RealtimeConfig::default()).with_pubsub(pubsub);

        for i in 0..100 {
            let sub = hub
                .subscribe("acme", &format!("ephemeral-{i}"))
                .await
                .expect("subscribe");
            drop(sub);
        }

        assert!(
            hub.inner.channels.read().is_empty(),
            "channels map must be empty after dropping all room subscriptions"
        );
        assert!(
            hub.inner.pubsub_tasks.lock().is_empty(),
            "pubsub_tasks map must be empty after dropping all room subscriptions"
        );
    }

    #[tokio::test]
    async fn wait_drained_wakes_promptly_when_last_connection_drops() {
        let hub = RealtimeHub::new(
            RealtimeConfig::default().with_shutdown_drain_timeout(Duration::from_secs(10)),
        );

        for _ in 0..50 {
            let guard = hub.track_connection();
            let hub_clone = hub.clone();
            let waiter = tokio::spawn(async move {
                let start = tokio::time::Instant::now();
                hub_clone.wait_drained().await;
                start.elapsed()
            });
            tokio::task::yield_now().await;
            drop(guard);

            let elapsed = tokio::time::timeout(Duration::from_secs(2), waiter)
                .await
                .expect("wait_drained must not stall on lost wakeup")
                .expect("waiter task should not panic");
            assert!(
                elapsed < Duration::from_secs(2),
                "wait_drained took {elapsed:?}, expected prompt wakeup well below 10s timeout"
            );
        }
    }

    #[tokio::test]
    async fn gated_pubsub_subscribe_during_initiate_shutdown_leaves_pubsub_tasks_empty() {
        use rume_messaging::pubsub::{PubSubError, PubSubMessage, PubSubSubscription};

        #[derive(Debug)]
        struct DummySubscription;

        impl PubSubSubscription for DummySubscription {
            fn recv(
                &mut self,
            ) -> rume_messaging::outbox::repository::BoxFuture<
                '_,
                Result<Option<PubSubMessage>, PubSubError>,
            > {
                Box::pin(async { std::future::pending().await })
            }
        }

        #[derive(Debug)]
        struct GatedPubSub {
            entered: Arc<Notify>,
            release: Arc<Notify>,
        }

        impl PubSub for GatedPubSub {
            fn publish<'a>(
                &'a self,
                _topic: &'a str,
                _payload: &'a [u8],
            ) -> rume_messaging::outbox::repository::BoxFuture<'a, Result<(), PubSubError>>
            {
                Box::pin(async { Ok(()) })
            }

            fn subscribe<'a>(
                &'a self,
                _topic: &'a str,
            ) -> rume_messaging::outbox::repository::BoxFuture<
                'a,
                Result<Box<dyn PubSubSubscription>, PubSubError>,
            > {
                let entered = Arc::clone(&self.entered);
                let release = Arc::clone(&self.release);
                Box::pin(async move {
                    entered.notify_one();
                    release.notified().await;
                    Ok(Box::new(DummySubscription) as Box<dyn PubSubSubscription>)
                })
            }
        }

        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let pubsub = Arc::new(GatedPubSub {
            entered: Arc::clone(&entered),
            release: Arc::clone(&release),
        });
        let hub = RealtimeHub::new(RealtimeConfig::default()).with_pubsub(pubsub);

        let hub_clone = hub.clone();
        let sub_task = tokio::spawn(async move { hub_clone.subscribe("acme", "room-1").await });

        entered.notified().await;
        hub.initiate_shutdown();
        release.notify_one();

        let res = sub_task.await.expect("subscribe task must not panic");
        assert!(
            matches!(res, Err(RealtimeError::ShuttingDown)),
            "expected ShuttingDown when shutdown races with pubsub.subscribe, got {res:?}"
        );
        assert!(
            hub.inner.pubsub_tasks.lock().is_empty(),
            "pubsub_tasks must remain empty after shutdown race"
        );
    }
}
