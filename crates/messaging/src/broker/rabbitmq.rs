//! RabbitMQ [`MessageBroker`] implementation via `lapin`.

use std::collections::{BTreeMap, VecDeque};
use std::fmt::Debug;
use std::future::Future;
use std::sync::Arc;

use futures_util::StreamExt;
use lapin::BasicProperties;
use lapin::Confirmation;
use lapin::options::{
    BasicAckOptions, BasicConsumeOptions, BasicNackOptions, BasicPublishOptions,
    ConfirmSelectOptions, QueueDeclareOptions,
};
use lapin::types::{AMQPValue, FieldTable, LongString, ShortString};
use tokio::sync::Mutex;

use crate::broker::{
    BoxFuture, BrokerConsumer, BrokerError, BrokerMessage, BrokerPublisher, MessageBroker,
};

/// Persistent AMQP delivery mode (`2` = write message to disk on durable queues).
const AMQP_DELIVERY_MODE_PERSISTENT: u8 = 2;

/// RabbitMQ-backed [`MessageBroker`].
#[derive(Clone)]
pub struct RabbitMqBroker {
    connection: Arc<lapin::Connection>,
    /// Pooled publisher channel with publisher confirms enabled, recreated only if closed.
    publish_channel: Arc<Mutex<Option<lapin::Channel>>>,
}

impl Debug for RabbitMqBroker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RabbitMqBroker").finish_non_exhaustive()
    }
}

impl RabbitMqBroker {
    /// Wraps an existing `lapin::Connection`.
    pub fn new(connection: Arc<lapin::Connection>) -> Self {
        Self {
            connection,
            publish_channel: Arc::new(Mutex::new(None)),
        }
    }

    /// Connects to a RabbitMQ broker at `uri`.
    pub async fn connect(uri: &str) -> Result<Self, BrokerError> {
        let connection = lapin::Connection::connect(uri, lapin::ConnectionProperties::default())
            .await
            .map_err(|e| BrokerError::Transport(e.to_string()))?;
        Ok(Self::new(Arc::new(connection)))
    }

    /// Returns a reference to the underlying `lapin::Connection`.
    pub fn connection(&self) -> &lapin::Connection {
        &self.connection
    }

    /// Returns the pooled publisher channel with publisher confirms enabled, creating a new
    /// channel only when none exists yet or the cached channel is no longer connected.
    async fn acquire_publish_channel(&self) -> Result<lapin::Channel, BrokerError> {
        let mut guard = self.publish_channel.lock().await;
        if let Some(ref ch) = *guard
            && ch.status().connected()
        {
            return Ok(ch.clone());
        }
        let ch = self
            .connection
            .create_channel()
            .await
            .map_err(|e| BrokerError::Transport(e.to_string()))?;
        ch.confirm_select(ConfirmSelectOptions::default())
            .await
            .map_err(|e| BrokerError::Transport(e.to_string()))?;
        *guard = Some(ch.clone());
        Ok(ch)
    }
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
            let channel = self.acquire_publish_channel().await?;

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
                        *self.publish_channel.lock().await = None;
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
                            *self.publish_channel.lock().await = None;
                        }
                        return Err(err);
                    }
                }
                Err(e) => {
                    if !channel.status().connected() {
                        *self.publish_channel.lock().await = None;
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
            let channel = self
                .connection
                .create_channel()
                .await
                .map_err(|e| BrokerError::Transport(e.to_string()))?;

            channel
                .queue_declare(
                    topic.into(),
                    QueueDeclareOptions::default(),
                    FieldTable::default(),
                )
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
