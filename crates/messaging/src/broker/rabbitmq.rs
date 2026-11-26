//! RabbitMQ [`MessageBroker`] implementation via `lapin`.

use std::collections::BTreeMap;
use std::fmt::Debug;
use std::sync::Arc;

use futures_util::StreamExt;
use lapin::BasicProperties;
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

struct RabbitMqConsumer {
    topic: String,
    channel: lapin::Channel,
    consumer: lapin::Consumer,
    pending_delivery_tag: Option<u64>,
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
                    // tag so the caller explicitly invokes `ack()` after processing or
                    // `nack(requeue)` on failure, preventing message loss on crash.
                    self.pending_delivery_tag = Some(delivery.delivery_tag);
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
            if let Some(tag) = self.pending_delivery_tag.take() {
                self.channel
                    .basic_ack(tag, BasicAckOptions::default())
                    .await
                    .map_err(|e| BrokerError::Transport(e.to_string()))?;
            }
            Ok(())
        })
    }

    fn nack(&mut self, requeue: bool) -> BoxFuture<'_, Result<(), BrokerError>> {
        Box::pin(async move {
            if let Some(tag) = self.pending_delivery_tag.take() {
                self.channel
                    .basic_nack(
                        tag,
                        BasicNackOptions {
                            requeue,
                            ..Default::default()
                        },
                    )
                    .await
                    .map_err(|e| BrokerError::Transport(e.to_string()))?;
            }
            Ok(())
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

            if let Err(e) = confirm.await {
                if !channel.status().connected() {
                    *self.publish_channel.lock().await = None;
                }
                return Err(BrokerError::Transport(e.to_string()));
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
                pending_delivery_tag: None,
            }) as Box<dyn BrokerConsumer>)
        })
    }
}
