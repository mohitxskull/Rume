//! Distributed event bus bridging local [`EventBus`] with [`PubSub`].

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::events::bus::EventBus;
use crate::events::error::EventError;
use crate::events::event::Event;
use crate::pubsub::PubSub;

/// Wire envelope published across nodes by [`DistributedEventBus`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DistributedEventEnvelope {
    /// Unique identifier of the originating node, used for echo deduplication.
    pub origin_node_id: Uuid,
    /// Canonical event type name (`Event::TYPE`).
    pub event_type: String,
    /// Serialized JSON event payload.
    pub payload: Vec<u8>,
    /// Optional W3C `traceparent` propagated across nodes.
    pub trace_context: Option<String>,
}

/// Distributed event bus bridging a local [`EventBus`] with a [`PubSub`] backend.
#[derive(Debug, Clone)]
pub struct DistributedEventBus {
    node_id: Uuid,
    local: EventBus,
    pubsub: Arc<dyn PubSub>,
}

impl DistributedEventBus {
    /// Creates a new distributed event bus with a fresh unique `node_id`.
    pub fn new(local: EventBus, pubsub: Arc<dyn PubSub>) -> Self {
        Self {
            node_id: Uuid::new_v4(),
            local,
            pubsub,
        }
    }

    /// Returns this node's unique identifier.
    pub fn node_id(&self) -> Uuid {
        self.node_id
    }

    /// Returns a reference to the underlying local [`EventBus`].
    pub fn local(&self) -> &EventBus {
        &self.local
    }

    /// Returns a reference to the underlying [`PubSub`] backend.
    pub fn pubsub(&self) -> &dyn PubSub {
        self.pubsub.as_ref()
    }

    /// Publishes an event both locally and to remote nodes via [`PubSub`].
    pub async fn publish<E: Event + Serialize>(&self, event: &E) -> Result<(), EventError> {
        self.local.publish_sync(event).await?;

        let payload = serde_json::to_vec(event).map_err(|e| {
            EventError::Bus(format!("failed to serialize event '{}': {e}", E::TYPE))
        })?;
        let trace_context =
            rume_observability::TraceContext::current().map(|tc| tc.to_serialized());
        let envelope = DistributedEventEnvelope {
            origin_node_id: self.node_id,
            event_type: E::TYPE.to_string(),
            payload,
            trace_context,
        };
        let bytes = serde_json::to_vec(&envelope).map_err(|e| {
            EventError::Bus(format!(
                "failed to serialize distributed envelope for '{}': {e}",
                E::TYPE
            ))
        })?;

        self.pubsub
            .publish(E::TYPE, &bytes)
            .await
            .map_err(|e| EventError::Bus(e.to_string()))?;
        Ok(())
    }

    /// Subscribes to remote events of type `E` and dispatches them to the local [`EventBus`],
    /// ignoring echoes from `self.node_id`.
    pub async fn bridge<E: Event + for<'de> Deserialize<'de>>(&self) -> Result<(), EventError> {
        let mut sub = self
            .pubsub
            .subscribe(E::TYPE)
            .await
            .map_err(|e| EventError::Bus(e.to_string()))?;
        let node_id = self.node_id;
        let local = self.local.clone();

        tokio::spawn(async move {
            loop {
                match sub.recv().await {
                    Ok(Some(msg)) => {
                        let envelope: DistributedEventEnvelope =
                            match serde_json::from_slice(&msg.payload) {
                                Ok(env) => env,
                                Err(e) => {
                                    tracing::warn!(
                                        event.type = E::TYPE,
                                        "failed to deserialize distributed event envelope: {e}"
                                    );
                                    continue;
                                }
                            };

                        if envelope.origin_node_id == node_id {
                            continue;
                        }

                        let event: E = match serde_json::from_slice(&envelope.payload) {
                            Ok(ev) => ev,
                            Err(e) => {
                                tracing::warn!(
                                    event.type = E::TYPE,
                                    origin_node_id = %envelope.origin_node_id,
                                    "failed to deserialize distributed event payload: {e}"
                                );
                                continue;
                            }
                        };

                        let tc = envelope
                            .trace_context
                            .as_deref()
                            .and_then(rume_observability::TraceContext::parse_serialized);

                        let dispatch_res = if let Some(tc) = tc {
                            let span = tracing::info_span!(
                                "distributed_event.dispatch",
                                event.type = E::TYPE,
                                origin_node_id = %envelope.origin_node_id,
                            );
                            tc.scope_future(span, local.publish_sync(&event)).await
                        } else {
                            local.publish_sync(&event).await
                        };

                        if let Err(e) = dispatch_res {
                            tracing::error!(
                                event.type = E::TYPE,
                                origin_node_id = %envelope.origin_node_id,
                                "failed to dispatch distributed event locally: {e}"
                            );
                        }
                    }
                    Ok(None) => break,
                    Err(e) => {
                        tracing::warn!(
                            event.type = E::TYPE,
                            "distributed event bridge subscription ended: {e}"
                        );
                        break;
                    }
                }
            }
        });

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pubsub::InMemoryPubSub;
    use rume_observability::TraceContext;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    use tokio::sync::oneshot;

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    struct OrderPlaced {
        order_id: String,
    }

    impl Event for OrderPlaced {
        const TYPE: &'static str = "order.placed";
    }

    #[tokio::test]
    async fn distributed_event_bus_propagates_events_across_nodes_without_echo() {
        let _sub_lock = crate::events::bus::tests::SUBSCRIBER_TEST_LOCK.lock().await;
        let pubsub: Arc<dyn PubSub> = Arc::new(InMemoryPubSub::new());

        let node_a = DistributedEventBus::new(EventBus::new(), Arc::clone(&pubsub));
        let node_b = DistributedEventBus::new(EventBus::new(), Arc::clone(&pubsub));

        let a_count = Arc::new(AtomicUsize::new(0));
        let a_count_clone = Arc::clone(&a_count);
        node_a
            .local()
            .subscribe_fn::<OrderPlaced, _, _>(move |_evt| {
                let count = Arc::clone(&a_count_clone);
                async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }
            });

        let b_count = Arc::new(AtomicUsize::new(0));
        let b_count_clone = Arc::clone(&b_count);
        let (b_tx, b_rx) = oneshot::channel::<OrderPlaced>();
        let b_tx = Arc::new(tokio::sync::Mutex::new(Some(b_tx)));
        node_b
            .local()
            .subscribe_fn::<OrderPlaced, _, _>(move |evt| {
                let count = Arc::clone(&b_count_clone);
                let tx = Arc::clone(&b_tx);
                let evt = evt.clone();
                async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    if let Some(sender) = tx.lock().await.take() {
                        let _ = sender.send(evt);
                    }
                    Ok(())
                }
            });

        node_a.bridge::<OrderPlaced>().await.unwrap();
        node_b.bridge::<OrderPlaced>().await.unwrap();

        let event = OrderPlaced {
            order_id: "ord-42".into(),
        };
        node_a.publish(&event).await.unwrap();

        let received = tokio::time::timeout(Duration::from_secs(2), b_rx)
            .await
            .expect("node_b timed out waiting for event")
            .expect("oneshot channel closed");
        assert_eq!(received, event);

        // Allow any stray echo task to settle.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(
            a_count.load(Ordering::SeqCst),
            1,
            "node_a must receive its own published event exactly once (local only, no remote echo)"
        );
        assert_eq!(
            b_count.load(Ordering::SeqCst),
            1,
            "node_b must receive the remote event exactly once"
        );
    }

    #[tokio::test]
    async fn distributed_event_bus_propagates_trace_context_across_nodes() {
        let _sub_lock = crate::events::bus::tests::SUBSCRIBER_TEST_LOCK.lock().await;
        let pubsub: Arc<dyn PubSub> = Arc::new(InMemoryPubSub::new());

        let node_a = DistributedEventBus::new(EventBus::new(), Arc::clone(&pubsub));
        let node_b = DistributedEventBus::new(EventBus::new(), Arc::clone(&pubsub));

        let (tc_tx, tc_rx) = oneshot::channel::<Option<TraceContext>>();
        let tc_tx = Arc::new(tokio::sync::Mutex::new(Some(tc_tx)));
        node_b
            .local()
            .subscribe_fn::<OrderPlaced, _, _>(move |_evt| {
                let tx = Arc::clone(&tc_tx);
                async move {
                    if let Some(sender) = tx.lock().await.take() {
                        let _ = sender.send(TraceContext::current());
                    }
                    Ok(())
                }
            });

        node_b.bridge::<OrderPlaced>().await.unwrap();

        let parent_tc = TraceContext::generate();
        let expected_trace_id = parent_tc.trace_id().to_string();

        parent_tc
            .scope_future(tracing::info_span!("node_a.publish"), async {
                node_a
                    .publish(&OrderPlaced {
                        order_id: "ord-trace".into(),
                    })
                    .await
                    .unwrap();
            })
            .await;

        let observed_tc = tokio::time::timeout(Duration::from_secs(2), tc_rx)
            .await
            .expect("node_b timed out waiting for traced event")
            .expect("oneshot channel closed")
            .expect("node_b listener must observe TraceContext::current()");
        assert_eq!(observed_tc.trace_id(), expected_trace_id);
    }
}
