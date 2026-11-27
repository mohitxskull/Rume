//! In-memory [`MessageBroker`] for hermetic tests and local development.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use tokio::sync::{RwLock, mpsc};

use crate::broker::{
    BoxFuture, BrokerConsumer, BrokerError, BrokerMessage, BrokerPublisher, MessageBroker,
};

/// Default per-subscriber bounded channel capacity.
const DEFAULT_CHANNEL_CAPACITY: usize = 1024;

type TopicSubscribers = HashMap<String, Vec<mpsc::Sender<BrokerMessage>>>;

/// In-memory [`MessageBroker`] implementation.
#[derive(Debug, Clone)]
pub struct InMemoryBroker {
    subscribers: Arc<RwLock<TopicSubscribers>>,
    capacity: usize,
}

impl Default for InMemoryBroker {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryBroker {
    /// Creates a new in-memory message broker with the default per-subscriber capacity (`1024`).
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_CHANNEL_CAPACITY)
    }

    /// Creates a new in-memory message broker with a custom per-subscriber bounded channel capacity.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            subscribers: Arc::new(RwLock::new(HashMap::new())),
            capacity: capacity.max(1),
        }
    }
}

#[derive(Debug)]
struct InMemoryConsumer {
    rx: mpsc::Receiver<BrokerMessage>,
    requeued: VecDeque<BrokerMessage>,
    in_flight: VecDeque<BrokerMessage>,
}

impl BrokerConsumer for InMemoryConsumer {
    fn next_message(&mut self) -> BoxFuture<'_, Result<Option<BrokerMessage>, BrokerError>> {
        Box::pin(async move {
            let msg = if let Some(m) = self.requeued.pop_front() {
                Some(m)
            } else {
                self.rx.recv().await
            };
            if let Some(ref m) = msg {
                self.in_flight.push_back(m.clone());
            }
            Ok(msg)
        })
    }

    fn ack(&mut self) -> BoxFuture<'_, Result<(), BrokerError>> {
        Box::pin(async move {
            self.in_flight.pop_front();
            Ok(())
        })
    }

    fn nack(&mut self, requeue: bool) -> BoxFuture<'_, Result<(), BrokerError>> {
        Box::pin(async move {
            if let Some(msg) = self.in_flight.pop_front()
                && requeue
            {
                self.requeued.push_front(msg);
            }
            Ok(())
        })
    }
}

impl BrokerPublisher for InMemoryBroker {
    fn publish(&self, message: BrokerMessage) -> BoxFuture<'_, Result<(), BrokerError>> {
        Box::pin(async move {
            // Why: clone bounded `mpsc::Sender`s under a short read lock and drop the lock
            // before awaiting `send().await` so slow consumers applying backpressure never
            // stall `subscribe()` or publishers on other topics (`RC-38`).
            let senders = {
                let map = self.subscribers.read().await;
                map.get(&message.topic).cloned().unwrap_or_default()
            };
            let mut any_closed = false;
            for tx in senders {
                if tx.send(message.clone()).await.is_err() {
                    any_closed = true;
                }
            }
            if any_closed {
                let mut map = self.subscribers.write().await;
                if let Some(list) = map.get_mut(&message.topic) {
                    list.retain(|tx| !tx.is_closed());
                }
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
            let (tx, rx) = mpsc::channel(self.capacity);
            let mut map = self.subscribers.write().await;
            map.entry(topic.to_string()).or_default().push(tx);
            Ok(Box::new(InMemoryConsumer {
                rx,
                requeued: VecDeque::new(),
                in_flight: VecDeque::new(),
            }) as Box<dyn BrokerConsumer>)
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
        consumer.ack().await.expect("ack");
    }

    #[tokio::test]
    async fn in_memory_broker_explicit_nack_requeues_message() {
        let broker = InMemoryBroker::new();
        let mut consumer = broker.subscribe("orders.retry").await.expect("subscribe");

        broker
            .publish(BrokerMessage {
                topic: "orders.retry".to_string(),
                payload: br#"{"attempt":1}"#.to_vec(),
                headers: BTreeMap::new(),
            })
            .await
            .expect("publish");

        let first = consumer
            .next_message()
            .await
            .expect("first next_message")
            .expect("message present");
        assert_eq!(first.payload, br#"{"attempt":1}"#);

        consumer.nack(true).await.expect("nack with requeue");

        let redelivered = consumer
            .next_message()
            .await
            .expect("second next_message")
            .expect("redelivered message");
        assert_eq!(redelivered.payload, br#"{"attempt":1}"#);
        consumer.ack().await.expect("ack after redelivery");
    }

    #[tokio::test]
    async fn in_memory_broker_preserves_fifo_in_flight_and_bounded_backpressure() {
        let broker = InMemoryBroker::with_capacity(2);
        let mut consumer = broker.subscribe("orders.fifo").await.expect("subscribe");

        for i in 1u8..=2 {
            broker
                .publish(BrokerMessage {
                    topic: "orders.fifo".to_string(),
                    payload: vec![i],
                    headers: BTreeMap::new(),
                })
                .await
                .expect("publish within capacity");
        }

        // Third publish must block while the bounded channel (capacity 2) is full.
        let broker_clone = broker.clone();
        let blocked_pub = tokio::spawn(async move {
            broker_clone
                .publish(BrokerMessage {
                    topic: "orders.fifo".to_string(),
                    payload: vec![3],
                    headers: BTreeMap::new(),
                })
                .await
        });
        tokio::task::yield_now().await;
        assert!(
            !blocked_pub.is_finished(),
            "publish past bounded channel capacity must apply backpressure"
        );

        // Consume two messages without acking yet; FIFO VecDeque tracks both.
        let m1 = consumer.next_message().await.unwrap().unwrap();
        let m2 = consumer.next_message().await.unwrap().unwrap();
        assert_eq!(m1.payload, vec![1]);
        assert_eq!(m2.payload, vec![2]);

        // Unblocked third publish now completes.
        blocked_pub.await.unwrap().expect("third publish completes");

        // Nacking requeues the oldest in-flight message (m1), not m2.
        consumer.nack(true).await.expect("nack m1");
        let redelivered = consumer.next_message().await.unwrap().unwrap();
        assert_eq!(redelivered.payload, vec![1]);
        // Ack m2, then m1 (re-pushed at back), then consume m3.
        consumer.ack().await.expect("ack m2");
        consumer.ack().await.expect("ack redelivered m1");

        let m3 = consumer.next_message().await.unwrap().unwrap();
        assert_eq!(m3.payload, vec![3]);
        consumer.ack().await.expect("ack m3");
    }
}
