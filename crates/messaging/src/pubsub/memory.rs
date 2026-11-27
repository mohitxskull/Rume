//! In-memory [`PubSub`] backend backed by `tokio::sync::broadcast`.

use std::collections::HashMap;
use std::sync::RwLock;

use tokio::sync::broadcast;

use crate::pubsub::{BoxFuture, PubSub, PubSubError, PubSubMessage, PubSubSubscription};

const DEFAULT_CHANNEL_CAPACITY: usize = 1024;

/// In-memory [`PubSub`] implementation for hermetic tests and single-node deployments.
#[derive(Debug)]
pub struct InMemoryPubSub {
    capacity: usize,
    channels: RwLock<HashMap<String, broadcast::Sender<PubSubMessage>>>,
}

impl Default for InMemoryPubSub {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryPubSub {
    /// Creates a new in-memory pub/sub hub with default per-topic capacity (`1024`).
    pub fn new() -> Self {
        Self {
            capacity: DEFAULT_CHANNEL_CAPACITY,
            channels: RwLock::new(HashMap::new()),
        }
    }

    /// Creates a new in-memory pub/sub hub with the given per-topic broadcast capacity.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            channels: RwLock::new(HashMap::new()),
        }
    }

    fn sender_for(&self, topic: &str) -> Result<broadcast::Sender<PubSubMessage>, PubSubError> {
        {
            let guard = self
                .channels
                .read()
                .map_err(|e| PubSubError::Transport(format!("pubsub lock poisoned: {e}")))?;
            if let Some(tx) = guard.get(topic) {
                return Ok(tx.clone());
            }
        }

        let mut guard = self
            .channels
            .write()
            .map_err(|e| PubSubError::Transport(format!("pubsub lock poisoned: {e}")))?;
        let tx = guard
            .entry(topic.to_string())
            .or_insert_with(|| broadcast::channel(self.capacity).0);
        Ok(tx.clone())
    }
}

#[derive(Debug)]
struct InMemorySubscription {
    rx: broadcast::Receiver<PubSubMessage>,
}

impl PubSubSubscription for InMemorySubscription {
    fn recv(&mut self) -> BoxFuture<'_, Result<Option<PubSubMessage>, PubSubError>> {
        Box::pin(async move {
            match self.rx.recv().await {
                Ok(msg) => Ok(Some(msg)),
                Err(broadcast::error::RecvError::Closed) => Ok(None),
                Err(broadcast::error::RecvError::Lagged(skipped)) => Err(PubSubError::Closed(
                    format!("in-memory subscription lagged by {skipped} messages"),
                )),
            }
        })
    }
}

impl PubSub for InMemoryPubSub {
    fn publish<'a>(
        &'a self,
        topic: &'a str,
        payload: &'a [u8],
    ) -> BoxFuture<'a, Result<(), PubSubError>> {
        Box::pin(async move {
            let tx = self.sender_for(topic)?;
            let msg = PubSubMessage {
                topic: topic.to_string(),
                payload: payload.to_vec(),
            };
            // Why: publishing to a topic with zero active subscribers is a normal no-op
            // in pub/sub semantics; `broadcast::Sender::send` only errors when no receivers exist.
            let _ = tx.send(msg);
            Ok(())
        })
    }

    fn subscribe<'a>(
        &'a self,
        topic: &'a str,
    ) -> BoxFuture<'a, Result<Box<dyn PubSubSubscription>, PubSubError>> {
        Box::pin(async move {
            let tx = self.sender_for(topic)?;
            let rx = tx.subscribe();
            Ok(Box::new(InMemorySubscription { rx }) as Box<dyn PubSubSubscription>)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn in_memory_pubsub_delivers_messages_to_multiple_subscribers() {
        let pubsub = InMemoryPubSub::new();

        let mut sub_a1 = pubsub.subscribe("orders.created").await.unwrap();
        let mut sub_a2 = pubsub.subscribe("orders.created").await.unwrap();
        let mut sub_b = pubsub.subscribe("users.registered").await.unwrap();

        pubsub.publish("orders.created", b"order-1").await.unwrap();
        pubsub.publish("users.registered", b"user-1").await.unwrap();

        let msg_a1 = tokio::time::timeout(Duration::from_secs(1), sub_a1.recv())
            .await
            .expect("sub_a1 timed out")
            .unwrap()
            .expect("sub_a1 expected message");
        assert_eq!(msg_a1.topic, "orders.created");
        assert_eq!(msg_a1.payload, b"order-1");

        let msg_a2 = tokio::time::timeout(Duration::from_secs(1), sub_a2.recv())
            .await
            .expect("sub_a2 timed out")
            .unwrap()
            .expect("sub_a2 expected message");
        assert_eq!(msg_a2.topic, "orders.created");
        assert_eq!(msg_a2.payload, b"order-1");

        let msg_b = tokio::time::timeout(Duration::from_secs(1), sub_b.recv())
            .await
            .expect("sub_b timed out")
            .unwrap()
            .expect("sub_b expected message");
        assert_eq!(msg_b.topic, "users.registered");
        assert_eq!(msg_b.payload, b"user-1");

        // Topic isolation: sub_a1 must not receive users.registered messages.
        let no_cross = tokio::time::timeout(Duration::from_millis(50), sub_a1.recv()).await;
        assert!(
            no_cross.is_err(),
            "sub_a1 must not receive messages from another topic"
        );
    }
}
