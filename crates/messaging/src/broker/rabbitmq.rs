//! RabbitMQ [`MessageBroker`] implementation via `lapin`.

use std::collections::BTreeMap;
use std::fmt::Debug;

use futures_util::StreamExt;
use lapin::BasicProperties;
use lapin::options::{
    BasicAckOptions, BasicConsumeOptions, BasicPublishOptions, QueueDeclareOptions,
};
use lapin::types::{AMQPValue, FieldTable, LongString, ShortString};

use crate::broker::{
    BoxFuture, BrokerConsumer, BrokerError, BrokerMessage, BrokerPublisher, MessageBroker,
};

/// RabbitMQ-backed [`MessageBroker`].
#[derive(Clone)]
pub struct RabbitMqBroker {
    connection: std::sync::Arc<lapin::Connection>,
}

impl Debug for RabbitMqBroker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RabbitMqBroker").finish_non_exhaustive()
    }
}

impl RabbitMqBroker {
    /// Wraps an existing `lapin::Connection`.
    pub fn new(connection: std::sync::Arc<lapin::Connection>) -> Self {
        Self { connection }
    }

    /// Connects to a RabbitMQ broker at `uri`.
    pub async fn connect(uri: &str) -> Result<Self, BrokerError> {
        let connection = lapin::Connection::connect(uri, lapin::ConnectionProperties::default())
            .await
            .map_err(|e| BrokerError::Transport(e.to_string()))?;
        Ok(Self {
            connection: std::sync::Arc::new(connection),
        })
    }

    /// Returns a reference to the underlying `lapin::Connection`.
    pub fn connection(&self) -> &lapin::Connection {
        &self.connection
    }
}

struct RabbitMqConsumer {
    topic: String,
    consumer: lapin::Consumer,
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
                    delivery
                        .ack(BasicAckOptions::default())
                        .await
                        .map_err(|e| BrokerError::Transport(e.to_string()))?;
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
}

impl BrokerPublisher for RabbitMqBroker {
    fn publish(&self, message: BrokerMessage) -> BoxFuture<'_, Result<(), BrokerError>> {
        Box::pin(async move {
            let channel = self
                .connection
                .create_channel()
                .await
                .map_err(|e| BrokerError::Transport(e.to_string()))?;

            let mut props = BasicProperties::default();
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

            channel
                .basic_publish(
                    "".into(),
                    message.topic.as_str().into(),
                    BasicPublishOptions::default(),
                    &message.payload,
                    props,
                )
                .await
                .map_err(|e| BrokerError::Transport(e.to_string()))?
                .await
                .map_err(|e| BrokerError::Transport(e.to_string()))?;

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
                consumer,
            }) as Box<dyn BrokerConsumer>)
        })
    }
}
