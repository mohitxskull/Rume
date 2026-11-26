//! NATS [`MessageBroker`] implementation via `async-nats`.

use std::collections::BTreeMap;

use futures_util::StreamExt;

use crate::broker::{
    BoxFuture, BrokerConsumer, BrokerError, BrokerMessage, BrokerPublisher, MessageBroker,
};

/// NATS-backed [`MessageBroker`].
#[derive(Debug, Clone)]
pub struct NatsBroker {
    client: async_nats::Client,
}

impl NatsBroker {
    /// Wraps an existing `async_nats::Client`.
    pub fn new(client: async_nats::Client) -> Self {
        Self { client }
    }

    /// Connects to a NATS server at `url`.
    pub async fn connect(url: &str) -> Result<Self, BrokerError> {
        let client = async_nats::connect(url)
            .await
            .map_err(|e| BrokerError::Transport(e.to_string()))?;
        Ok(Self { client })
    }

    /// Returns a reference to the underlying `async_nats::Client`.
    pub fn client(&self) -> &async_nats::Client {
        &self.client
    }
}

#[derive(Debug)]
struct NatsConsumer {
    subscriber: async_nats::Subscriber,
}

impl BrokerConsumer for NatsConsumer {
    fn next_message(&mut self) -> BoxFuture<'_, Result<Option<BrokerMessage>, BrokerError>> {
        Box::pin(async move {
            match self.subscriber.next().await {
                Some(msg) => {
                    let mut headers = BTreeMap::new();
                    if let Some(header_map) = msg.headers {
                        for (name, values) in header_map.iter() {
                            if let Some(first) = values.iter().next() {
                                headers.insert(name.to_string(), first.to_string());
                            }
                        }
                    }
                    Ok(Some(BrokerMessage {
                        topic: msg.subject.to_string(),
                        payload: msg.payload.to_vec(),
                        headers,
                    }))
                }
                None => Ok(None),
            }
        })
    }
}

impl BrokerPublisher for NatsBroker {
    fn publish(&self, message: BrokerMessage) -> BoxFuture<'_, Result<(), BrokerError>> {
        Box::pin(async move {
            if message.headers.is_empty() {
                self.client
                    .publish(message.topic, message.payload.into())
                    .await
                    .map_err(|e| BrokerError::Transport(e.to_string()))?;
            } else {
                let mut headers = async_nats::HeaderMap::new();
                for (key, value) in &message.headers {
                    headers.insert(key.as_str(), value.as_str());
                }
                self.client
                    .publish_with_headers(message.topic, headers, message.payload.into())
                    .await
                    .map_err(|e| BrokerError::Transport(e.to_string()))?;
            }
            Ok(())
        })
    }
}

impl MessageBroker for NatsBroker {
    fn subscribe<'a>(
        &'a self,
        topic: &'a str,
    ) -> BoxFuture<'a, Result<Box<dyn BrokerConsumer>, BrokerError>> {
        Box::pin(async move {
            let subscriber = self
                .client
                .subscribe(topic.to_string())
                .await
                .map_err(|e| BrokerError::Transport(e.to_string()))?;
            Ok(Box::new(NatsConsumer { subscriber }) as Box<dyn BrokerConsumer>)
        })
    }
}
