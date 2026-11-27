//! RabbitMQ [`MessageBroker`] implementation via `lapin`.

use std::collections::{BTreeMap, VecDeque};
use std::fmt::Debug;
use std::future::Future;
use std::sync::Arc;

use futures_util::StreamExt;
use lapin::BasicProperties;
use lapin::Confirmation;
use lapin::options::{
    BasicAckOptions, BasicConsumeOptions, BasicNackOptions, BasicPublishOptions, BasicQosOptions,
    ConfirmSelectOptions, QueueDeclareOptions,
};
use lapin::types::{AMQPValue, FieldTable, LongString, ShortString};
use tokio::sync::Mutex;

use crate::broker::{
    BoxFuture, BrokerConsumer, BrokerError, BrokerMessage, BrokerPublisher, MessageBroker,
};

/// Persistent AMQP delivery mode (`2` = write message to disk on durable queues).
const AMQP_DELIVERY_MODE_PERSISTENT: u8 = 2;

/// Default AMQP consumer prefetch limit bounding unacknowledged in-flight deliveries.
const DEFAULT_PREFETCH_COUNT: u16 = 32;

/// Unified connection and pooled publisher-channel state guarded by a single mutex.
///
/// Why: Keeping `connection`, monotonic `conn_gen`, and `Option<(u64, lapin::Channel)>`
/// behind one lock eliminates both reconnect races (`P2-04`):
/// 1. AMQP `u16` channel IDs reset to `1` on every new connection, so tagging cached channels
///    with `conn_gen` prevents a failing task from Connection N from evicting a healthy
///    channel with the same `u16` ID on Connection N+1.
/// 2. Updating `connection`, incrementing `conn_gen`, and clearing any older-generation
///    `publish_channel` atomically prevents a lock-drop window from wiping a newly installed
///    healthy channel.
struct RabbitMqState {
    connection: Arc<lapin::Connection>,
    conn_gen: u64,
    publish_channel: Option<(u64, lapin::Channel)>,
}

/// RabbitMQ-backed [`MessageBroker`].
#[derive(Clone)]
pub struct RabbitMqBroker {
    uri: Option<String>,
    state: Arc<Mutex<RabbitMqState>>,
    prefetch_count: u16,
    durable: bool,
}

impl Debug for RabbitMqBroker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RabbitMqBroker")
            .field("prefetch_count", &self.prefetch_count)
            .field("durable", &self.durable)
            .field("reconnectable", &self.uri.is_some())
            .finish_non_exhaustive()
    }
}

impl RabbitMqBroker {
    /// Wraps an existing `lapin::Connection` (non-reconnectable unless constructed via [`Self::connect`]).
    pub fn new(connection: Arc<lapin::Connection>) -> Self {
        Self {
            uri: None,
            state: Arc::new(Mutex::new(RabbitMqState {
                connection,
                conn_gen: 1,
                publish_channel: None,
            })),
            prefetch_count: DEFAULT_PREFETCH_COUNT,
            durable: true,
        }
    }

    /// Connects to a RabbitMQ broker at `uri` with automatic reconnect on connection drop.
    pub async fn connect(uri: &str) -> Result<Self, BrokerError> {
        let connection = lapin::Connection::connect(uri, lapin::ConnectionProperties::default())
            .await
            .map_err(|e| BrokerError::Transport(e.to_string()))?;
        Ok(Self {
            uri: Some(uri.to_string()),
            state: Arc::new(Mutex::new(RabbitMqState {
                connection: Arc::new(connection),
                conn_gen: 1,
                publish_channel: None,
            })),
            prefetch_count: DEFAULT_PREFETCH_COUNT,
            durable: true,
        })
    }

    /// Configures the AMQP `basic_qos` prefetch count for new consumers (default: `32`).
    pub fn with_prefetch_count(mut self, prefetch_count: u16) -> Self {
        self.prefetch_count = prefetch_count;
        self
    }

    /// Configures whether declared queues are durable (default: `true`).
    pub fn with_durable(mut self, durable: bool) -> Self {
        self.durable = durable;
        self
    }

    async fn connection_with_gen(&self) -> Result<(u64, Arc<lapin::Connection>), BrokerError> {
        let mut state = self.state.lock().await;
        if state.connection.status().connected() {
            return Ok((state.conn_gen, Arc::clone(&state.connection)));
        }
        let Some(ref uri) = self.uri else {
            return Err(BrokerError::Transport(
                "RabbitMQ connection is closed and no reconnect URI is configured".into(),
            ));
        };
        let reconnected = Arc::new(
            lapin::Connection::connect(uri, lapin::ConnectionProperties::default())
                .await
                .map_err(|e| BrokerError::Transport(e.to_string()))?,
        );
        state.conn_gen = state.conn_gen.wrapping_add(1);
        state.connection = Arc::clone(&reconnected);
        if state
            .publish_channel
            .as_ref()
            .is_some_and(|(cached_gen, _)| {
                should_clear_channel_on_reconnect(*cached_gen, state.conn_gen)
            })
        {
            state.publish_channel = None;
        }
        Ok((state.conn_gen, reconnected))
    }

    /// Returns a connected `lapin::Connection`, reconnecting transparently when `uri` is configured.
    pub async fn connection(&self) -> Result<Arc<lapin::Connection>, BrokerError> {
        self.connection_with_gen().await.map(|(_, conn)| conn)
    }

    /// Returns the pooled publisher channel tagged with its `conn_gen` and with publisher
    /// confirms enabled, creating a new channel only when none exists yet or the cached
    /// channel belongs to an older connection generation or is disconnected.
    async fn acquire_publish_channel(&self) -> Result<(u64, lapin::Channel), BrokerError> {
        {
            let state = self.state.lock().await;
            if state.connection.status().connected()
                && let Some((cached_gen, ref ch)) = state.publish_channel
                && cached_gen == state.conn_gen
                && ch.status().connected()
            {
                return Ok((cached_gen, ch.clone()));
            }
        }

        let (conn_gen, conn) = self.connection_with_gen().await?;
        let ch = conn
            .create_channel()
            .await
            .map_err(|e| BrokerError::Transport(e.to_string()))?;
        ch.confirm_select(ConfirmSelectOptions::default())
            .await
            .map_err(|e| BrokerError::Transport(e.to_string()))?;

        let mut state = self.state.lock().await;
        if state.conn_gen == conn_gen {
            let has_healthy = state
                .publish_channel
                .as_ref()
                .is_some_and(|(g, existing)| *g == conn_gen && existing.status().connected());
            if !has_healthy {
                state.publish_channel = Some((conn_gen, ch.clone()));
            }
        }
        Ok((conn_gen, ch))
    }

    /// Invalidates the cached publish channel only if it still matches `(failed_gen, failed_channel_id)`
    /// or is itself disconnected, so a failing task never evicts another task's healthy
    /// replacement channel (`RC-29`, `P2-04`).
    async fn invalidate_publish_channel(&self, failed_gen: u64, failed_channel_id: u16) {
        let mut state = self.state.lock().await;
        if state
            .publish_channel
            .as_ref()
            .is_some_and(|(cached_gen, ch)| {
                should_invalidate_channel(
                    *cached_gen,
                    ch.id(),
                    ch.status().connected(),
                    failed_gen,
                    failed_channel_id,
                )
            })
        {
            state.publish_channel = None;
        }
    }
}

/// Pure decision helper for `P2-04`: returns `true` iff a cached publish channel came from
/// an older connection generation than `new_conn_gen`.
fn should_clear_channel_on_reconnect(cached_gen: u64, new_conn_gen: u64) -> bool {
    cached_gen != new_conn_gen
}

/// Pure decision helper for `RC-29` / `P2-04`: returns `true` iff the cached channel is the
/// exact `(conn_gen, channel_id)` that failed or is no longer connected.
fn should_invalidate_channel(
    cached_gen: u64,
    cached_id: u16,
    cached_connected: bool,
    failed_gen: u64,
    failed_id: u16,
) -> bool {
    (cached_gen == failed_gen && cached_id == failed_id) || !cached_connected
}

/// Validates a RabbitMQ publisher confirmation, rejecting broker `Nack` or disabled confirms.
///
/// Why: `PublisherConfirm::await` resolves to `Ok(Confirmation::Nack(_))` when the broker
/// rejects a publish (and `Ok(Confirmation::NotRequested)` if confirms are not enabled).
/// Accepting any non-`Ack` variant as `Ok(())` would let `OutboxRelay` mark an undelivered
/// message as published.
fn verify_publish_confirmation(confirmation: &Confirmation) -> Result<(), BrokerError> {
    if confirmation.is_ack() {
        Ok(())
    } else if confirmation.is_nack() {
        Err(BrokerError::Transport(
            "RabbitMQ broker negatively acknowledged published message".into(),
        ))
    } else {
        Err(BrokerError::Transport(
            "RabbitMQ publisher confirms were not enabled on channel".into(),
        ))
    }
}

/// FIFO queue of unacknowledged AMQP delivery tags.
///
/// Why: a single `Option<u64>` silently overwrote earlier unacked tags if `next_message()` was
/// called more than once before `ack()`, and calling `.take()` before awaiting `basic_ack` /
/// `basic_nack` dropped the tag on transient transport errors. Peeking the front tag and popping
/// only after the broker call succeeds preserves delivery tags across retries.
#[derive(Debug, Default)]
struct PendingDeliveryTags {
    tags: VecDeque<u64>,
}

impl PendingDeliveryTags {
    fn push(&mut self, tag: u64) {
        self.tags.push_back(tag);
    }

    async fn consume_front<F, Fut>(&mut self, op: F) -> Result<(), BrokerError>
    where
        F: FnOnce(u64) -> Fut,
        Fut: Future<Output = Result<(), BrokerError>>,
    {
        if let Some(&tag) = self.tags.front() {
            op(tag).await?;
            self.tags.pop_front();
        }
        Ok(())
    }
}

struct RabbitMqConsumer {
    topic: String,
    channel: lapin::Channel,
    consumer: lapin::Consumer,
    pending_delivery_tags: PendingDeliveryTags,
}

impl Debug for RabbitMqConsumer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RabbitMqConsumer")
            .field("topic", &self.topic)
            .finish_non_exhaustive()
    }
}

impl BrokerConsumer for RabbitMqConsumer {
    fn next_message(&mut self) -> BoxFuture<'_, Result<Option<BrokerMessage>, BrokerError>> {
        Box::pin(async move {
            match self.consumer.next().await {
                Some(Ok(delivery)) => {
                    // Why: do NOT ack before yielding to the caller; retain the delivery
                    // tag in FIFO order so the caller explicitly invokes `ack()` after
                    // processing or `nack(requeue)` on failure, preventing message loss.
                    self.pending_delivery_tags.push(delivery.delivery_tag);
                    let mut headers = BTreeMap::new();
                    if let Some(table) = delivery.properties.headers() {
                        for (key, value) in table.inner() {
                            if let AMQPValue::LongString(s) = value {
                                headers.insert(key.to_string(), s.to_string());
                            }
                        }
                    }
                    Ok(Some(BrokerMessage {
                        topic: self.topic.clone(),
                        payload: delivery.data,
                        headers,
                    }))
                }
                Some(Err(err)) => Err(BrokerError::Transport(err.to_string())),
                None => Ok(None),
            }
        })
    }

    fn ack(&mut self) -> BoxFuture<'_, Result<(), BrokerError>> {
        Box::pin(async move {
            let channel = &self.channel;
            self.pending_delivery_tags
                .consume_front(|tag| async move {
                    channel
                        .basic_ack(tag, BasicAckOptions::default())
                        .await
                        .map_err(|e| BrokerError::Transport(e.to_string()))
                })
                .await
        })
    }

    fn nack(&mut self, requeue: bool) -> BoxFuture<'_, Result<(), BrokerError>> {
        Box::pin(async move {
            let channel = &self.channel;
            self.pending_delivery_tags
                .consume_front(|tag| async move {
                    channel
                        .basic_nack(
                            tag,
                            BasicNackOptions {
                                requeue,
                                ..Default::default()
                            },
                        )
                        .await
                        .map_err(|e| BrokerError::Transport(e.to_string()))
                })
                .await
        })
    }
}

impl BrokerPublisher for RabbitMqBroker {
    fn publish(&self, message: BrokerMessage) -> BoxFuture<'_, Result<(), BrokerError>> {
        Box::pin(async move {
            let (conn_gen, channel) = self.acquire_publish_channel().await?;

            let mut props =
                BasicProperties::default().with_delivery_mode(AMQP_DELIVERY_MODE_PERSISTENT);
            if !message.headers.is_empty() {
                let mut table = FieldTable::default();
                for (key, value) in &message.headers {
                    table.insert(
                        ShortString::from(key.as_str()),
                        AMQPValue::LongString(LongString::from(value.as_str())),
                    );
                }
                props = props.with_headers(table);
            }

            let publish_res = channel
                .basic_publish(
                    "".into(),
                    message.topic.as_str().into(),
                    BasicPublishOptions::default(),
                    &message.payload,
                    props,
                )
                .await;

            let confirm = match publish_res {
                Ok(c) => c,
                Err(e) => {
                    if !channel.status().connected() {
                        self.invalidate_publish_channel(conn_gen, channel.id())
                            .await;
                    }
                    return Err(BrokerError::Transport(e.to_string()));
                }
            };

            match confirm.await {
                Ok(confirmation) => {
                    if let Err(err) = verify_publish_confirmation(&confirmation) {
                        if !channel.status().connected()
                            || matches!(confirmation, Confirmation::NotRequested)
                        {
                            self.invalidate_publish_channel(conn_gen, channel.id())
                                .await;
                        }
                        return Err(err);
                    }
                }
                Err(e) => {
                    if !channel.status().connected() {
                        self.invalidate_publish_channel(conn_gen, channel.id())
                            .await;
                    }
                    return Err(BrokerError::Transport(e.to_string()));
                }
            }

            Ok(())
        })
    }
}

impl MessageBroker for RabbitMqBroker {
    fn subscribe<'a>(
        &'a self,
        topic: &'a str,
    ) -> BoxFuture<'a, Result<Box<dyn BrokerConsumer>, BrokerError>> {
        Box::pin(async move {
            let conn = self.connection().await?;
            let channel = conn
                .create_channel()
                .await
                .map_err(|e| BrokerError::Transport(e.to_string()))?;

            channel
                .queue_declare(
                    topic.into(),
                    QueueDeclareOptions {
                        durable: self.durable,
                        ..Default::default()
                    },
                    FieldTable::default(),
                )
                .await
                .map_err(|e| BrokerError::Transport(e.to_string()))?;

            // Why: bound unacknowledged in-flight deliveries per consumer so a slow
            // consumer cannot accumulate unbounded AMQP deliveries in `PendingDeliveryTags`
            // (`RC-10`).
            channel
                .basic_qos(self.prefetch_count, BasicQosOptions::default())
                .await
                .map_err(|e| BrokerError::Transport(e.to_string()))?;

            let consumer = channel
                .basic_consume(
                    topic.into(),
                    "".into(),
                    BasicConsumeOptions::default(),
                    FieldTable::default(),
                )
                .await
                .map_err(|e| BrokerError::Transport(e.to_string()))?;

            Ok(Box::new(RabbitMqConsumer {
                topic: topic.to_string(),
                channel,
                consumer,
                pending_delivery_tags: PendingDeliveryTags::default(),
            }) as Box<dyn BrokerConsumer>)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_invalidation_preserves_healthy_replacement_channel() {
        // Same (conn_gen, channel_id) that failed -> must invalidate even if status flickered.
        assert!(should_invalidate_channel(1, 1, true, 1, 1));
        assert!(should_invalidate_channel(1, 1, false, 1, 1));

        // Healthy replacement channel on the SAME connection with a different ID -> must NOT evict (`RC-29`).
        assert!(!should_invalidate_channel(1, 2, true, 1, 1));

        // Healthy replacement channel on a NEW connection generation with the SAME u16 ID `1`
        // (AMQP channel IDs reset to 1 on reconnect) -> must NOT evict (`P2-04`).
        assert!(!should_invalidate_channel(2, 1, true, 1, 1));

        // Disconnected replacement channel -> must invalidate regardless of generation/ID.
        assert!(should_invalidate_channel(1, 2, false, 1, 1));
        assert!(should_invalidate_channel(2, 1, false, 1, 1));

        // Reconnect clears channels from older generations and preserves current-generation channels.
        assert!(should_clear_channel_on_reconnect(1, 2));
        assert!(!should_clear_channel_on_reconnect(2, 2));
    }

    #[test]
    fn verify_publish_confirmation_accepts_ack_and_rejects_nack_or_not_requested() {
        assert!(verify_publish_confirmation(&Confirmation::Ack(None)).is_ok());

        let nack_err = verify_publish_confirmation(&Confirmation::Nack(None)).unwrap_err();
        assert!(
            matches!(nack_err, BrokerError::Transport(ref msg) if msg.contains("negatively acknowledged")),
            "expected transport error on Nack, got {nack_err:?}"
        );

        let not_req_err = verify_publish_confirmation(&Confirmation::NotRequested).unwrap_err();
        assert!(
            matches!(not_req_err, BrokerError::Transport(ref msg) if msg.contains("not enabled")),
            "expected transport error on NotRequested, got {not_req_err:?}"
        );
    }

    #[tokio::test]
    async fn pending_delivery_tags_preserves_fifo_order_and_retains_tag_on_failed_ack() {
        let mut pending = PendingDeliveryTags::default();
        pending.push(10);
        pending.push(11);

        // Simulated transient transport failure on ack of tag 10 must retain tag 10 at the front.
        let mut seen = Vec::new();
        let err = pending
            .consume_front(|tag| {
                seen.push(tag);
                async { Err(BrokerError::Transport("transient socket error".into())) }
            })
            .await
            .unwrap_err();
        assert!(matches!(err, BrokerError::Transport(_)));
        assert_eq!(seen, vec![10]);
        assert_eq!(pending.tags.front().copied(), Some(10));
        assert_eq!(pending.tags.len(), 2);

        // Retry succeeds: tag 10 is popped, leaving tag 11 next in FIFO order.
        pending
            .consume_front(|tag| {
                seen.push(tag);
                async { Ok(()) }
            })
            .await
            .unwrap();
        assert_eq!(seen, vec![10, 10]);
        assert_eq!(pending.tags.front().copied(), Some(11));

        // Second ack pops tag 11.
        pending
            .consume_front(|tag| {
                seen.push(tag);
                async { Ok(()) }
            })
            .await
            .unwrap();
        assert_eq!(seen, vec![10, 10, 11]);
        assert!(pending.tags.is_empty());
    }
}
