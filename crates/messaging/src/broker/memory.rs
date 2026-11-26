//! In-memory [`MessageBroker`] for hermetic tests and local development.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{RwLock, mpsc};

use crate::broker::{
    BoxFuture, BrokerConsumer, BrokerError, BrokerMessage, BrokerPublisher, MessageBroker,
};

type TopicSubscribers = HashMap<String, Vec<mpsc::UnboundedSender<BrokerMessage>>>;

/// In-memory [`MessageBroker`] implementation.
#[derive(Debug, Clone, Default)]
pub struct InMemoryBroker {
    subscribers: Arc<RwLock<TopicSubscribers>>,
}

impl InMemoryBroker {
    /// Creates a new in-memory message broker.
    pub fn new() -> Self {
        Self::default()
    }
}

#[derive(Debug)]
struct InMemoryConsumer {
    rx: mpsc::UnboundedReceiver<BrokerMessage>,
}

impl BrokerConsumer for InMemoryConsumer {
    fn next_message(&mut self) -> BoxFuture<'_, Result<Option<BrokerMessage>, BrokerError>> {
        Box::pin(async move { Ok(self.rx.recv().await) })
    }
}

impl BrokerPublisher for InMemoryBroker {
    fn publish(&self, message: BrokerMessage) -> BoxFuture<'_, Result<(), BrokerError>> {
        Box::pin(async move {
            let mut map = self.subscribers.write().await;
            if let Some(senders) = map.get_mut(&message.topic) {
                senders.retain(|tx| tx.send(message.clone()).is_ok());
            }
            Ok(())
        })
    }
}

impl MessageBroker for InMemoryBroker {
    fn subscribe<'a>(
        &'a self,
        topic: &'a str,
    ) -> BoxFuture<'a, Result<Box<dyn BrokerConsumer>, BrokerError>> {
        Box::pin(async move {
            let (tx, rx) = mpsc::unbounded_channel();
            let mut map = self.subscribers.write().await;
            map.entry(topic.to_string()).or_default().push(tx);
            Ok(Box::new(InMemoryConsumer { rx }) as Box<dyn BrokerConsumer>)
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    #[tokio::test]
    async fn in_memory_broker_publishes_and_consumes_with_headers() {
        let broker = InMemoryBroker::new();
        let mut consumer = broker.subscribe("orders.created").await.expect("subscribe");

        let mut headers = BTreeMap::new();
        headers.insert(
            "traceparent".to_string(),
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01".to_string(),
        );
        headers.insert("event_type".to_string(), "orders.created".to_string());

        broker
            .publish(BrokerMessage {
                topic: "orders.created".to_string(),
                payload: br#"{"order_id":42}"#.to_vec(),
                headers: headers.clone(),
            })
            .await
            .expect("publish");

        let received = consumer
            .next_message()
            .await
            .expect("next_message")
            .expect("some message");
        assert_eq!(received.topic, "orders.created");
        assert_eq!(received.payload, br#"{"order_id":42}"#);
        assert_eq!(received.headers, headers);
    }
}
