//! Distributed event bus bridging local [`EventBus`] with [`PubSub`].

use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use uuid::Uuid;

use crate::events::bus::EventBus;
use crate::events::error::EventError;
use crate::events::event::Event;
use crate::pubsub::{PubSub, PubSubError};

const BRIDGE_INITIAL_BACKOFF: Duration = Duration::from_millis(50);
const BRIDGE_MAX_BACKOFF: Duration = Duration::from_secs(5);

/// Supervised handle for a background [`DistributedEventBus::bridge`] task (`RC-13`).
///
/// Dropping the handle aborts the background task; call [`Self::shutdown`] to stop the
/// bridge gracefully and await task termination.
#[must_use = "dropping BridgeHandle aborts the background event bridge"]
#[derive(Debug)]
pub struct BridgeHandle {
    shutdown_tx: watch::Sender<bool>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl BridgeHandle {
    /// Signals shutdown and awaits the background bridge task to exit.
    pub async fn shutdown(mut self) {
        let _ = self.shutdown_tx.send(true);
        if let Some(handle) = self.task.take() {
            let _ = handle.await;
        }
    }
}

impl Drop for BridgeHandle {
    fn drop(&mut self) {
        let _ = self.shutdown_tx.send(true);
        if let Some(handle) = self.task.take() {
            handle.abort();
        }
    }
}

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
    ///
    /// Returns a supervised [`BridgeHandle`] whose background task automatically reconnects
    /// with bounded exponential backoff on [`PubSubError::Transport`] and shuts down cleanly
    /// on [`BridgeHandle::shutdown`] or drop (`RC-13`).
    pub async fn bridge<E: Event + for<'de> Deserialize<'de>>(
        &self,
    ) -> Result<BridgeHandle, EventError> {
        let mut sub = self
            .pubsub
            .subscribe(E::TYPE)
            .await
            .map_err(|e| EventError::Bus(e.to_string()))?;
        let node_id = self.node_id;
        let local = self.local.clone();
        let pubsub = Arc::clone(&self.pubsub);
        let (shutdown_tx, mut shutdown_rx) = watch::channel(false);

        let task = tokio::spawn(async move {
            let mut backoff = BRIDGE_INITIAL_BACKOFF;
            loop {
                let recv_res = tokio::select! {
                    biased;
                    _ = shutdown_rx.changed() => {
                        if *shutdown_rx.borrow() {
                            break;
                        }
                        continue;
                    }
                    res = sub.recv() => res,
                };

                match recv_res {
                    Ok(Some(msg)) => {
                        backoff = BRIDGE_INITIAL_BACKOFF;
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
                    Ok(None) => {
                        tracing::info!(
                            event.type = E::TYPE,
                            "distributed event bridge subscription closed"
                        );
                        break;
                    }
                    Err(PubSubError::Transport(e)) => {
                        tracing::warn!(
                            event.type = E::TYPE,
                            error = %e,
                            backoff_ms = backoff.as_millis() as u64,
                            "distributed event bridge transport error; re-subscribing"
                        );
                        // Re-subscribe loop with bounded exponential backoff until connected or shut down.
                        let reconnected = loop {
                            tokio::select! {
                                biased;
                                _ = shutdown_rx.changed() => {
                                    if *shutdown_rx.borrow() {
                                        break None;
                                    }
                                    continue;
                                }
                                _ = tokio::time::sleep(backoff) => {}
                            }
                            let sub_res = tokio::select! {
                                biased;
                                res = shutdown_rx.changed() => {
                                    if res.is_err() || *shutdown_rx.borrow() {
                                        break None;
                                    }
                                    continue;
                                }
                                res = pubsub.subscribe(E::TYPE) => res,
                            };
                            match sub_res {
                                Ok(new_sub) => {
                                    backoff = BRIDGE_INITIAL_BACKOFF;
                                    break Some(new_sub);
                                }
                                Err(PubSubError::Transport(sub_err)) => {
                                    backoff = (backoff * 2).min(BRIDGE_MAX_BACKOFF);
                                    tracing::warn!(
                                        event.type = E::TYPE,
                                        error = %sub_err,
                                        next_backoff_ms = backoff.as_millis() as u64,
                                        "distributed event bridge reconnect failed; retrying"
                                    );
                                }
                                Err(fatal_err) => {
                                    tracing::error!(
                                        event.type = E::TYPE,
                                        error = %fatal_err,
                                        "distributed event bridge encountered non-transport error during reconnect; stopping"
                                    );
                                    break None;
                                }
                            }
                        };
                        match reconnected {
                            Some(new_sub) => sub = new_sub,
                            None => break,
                        }
                    }
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

        Ok(BridgeHandle {
            shutdown_tx,
            task: Some(task),
        })
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

        let _bridge_a = node_a.bridge::<OrderPlaced>().await.unwrap();
        let _bridge_b = node_b.bridge::<OrderPlaced>().await.unwrap();

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

        let _bridge_b = node_b.bridge::<OrderPlaced>().await.unwrap();

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

    #[tokio::test]
    async fn distributed_event_bridge_reconnects_after_transient_transport_error() {
        use crate::outbox::repository::BoxFuture;
        use crate::pubsub::{PubSubMessage, PubSubSubscription};

        #[derive(Debug)]
        struct FailingOnceSub;
        impl PubSubSubscription for FailingOnceSub {
            fn recv(&mut self) -> BoxFuture<'_, Result<Option<PubSubMessage>, PubSubError>> {
                Box::pin(async { Err(PubSubError::Transport("simulated socket drop".into())) })
            }
        }

        #[derive(Debug)]
        struct SingleMessageSub {
            msg: Option<PubSubMessage>,
        }
        impl PubSubSubscription for SingleMessageSub {
            fn recv(&mut self) -> BoxFuture<'_, Result<Option<PubSubMessage>, PubSubError>> {
                let next = self.msg.take();
                Box::pin(async move {
                    if let Some(m) = next {
                        Ok(Some(m))
                    } else {
                        std::future::pending().await
                    }
                })
            }
        }

        #[derive(Debug)]
        struct ReconnectingPubSub {
            attempts: AtomicUsize,
            payload: Vec<u8>,
        }
        impl PubSub for ReconnectingPubSub {
            fn publish<'a>(
                &'a self,
                _topic: &'a str,
                _payload: &'a [u8],
            ) -> BoxFuture<'a, Result<(), PubSubError>> {
                Box::pin(async { Ok(()) })
            }

            fn subscribe<'a>(
                &'a self,
                topic: &'a str,
            ) -> BoxFuture<'a, Result<Box<dyn PubSubSubscription>, PubSubError>> {
                let attempt = self.attempts.fetch_add(1, Ordering::SeqCst);
                let topic = topic.to_string();
                let payload = self.payload.clone();
                Box::pin(async move {
                    match attempt {
                        0 => Ok(Box::new(FailingOnceSub) as Box<dyn PubSubSubscription>),
                        1 => Err(PubSubError::Transport("broker still restarting".into())),
                        _ => Ok(Box::new(SingleMessageSub {
                            msg: Some(PubSubMessage { topic, payload }),
                        }) as Box<dyn PubSubSubscription>),
                    }
                })
            }
        }

        let _sub_lock = crate::events::bus::tests::SUBSCRIBER_TEST_LOCK.lock().await;
        let expected_event = OrderPlaced {
            order_id: "ord-after-reconnect".into(),
        };
        let envelope = DistributedEventEnvelope {
            origin_node_id: Uuid::new_v4(),
            event_type: OrderPlaced::TYPE.to_string(),
            payload: serde_json::to_vec(&expected_event).unwrap(),
            trace_context: None,
        };
        let pubsub = Arc::new(ReconnectingPubSub {
            attempts: AtomicUsize::new(0),
            payload: serde_json::to_vec(&envelope).unwrap(),
        });

        let bus = DistributedEventBus::new(EventBus::new(), pubsub.clone());
        let (tx, rx) = oneshot::channel::<OrderPlaced>();
        let tx = Arc::new(tokio::sync::Mutex::new(Some(tx)));
        bus.local().subscribe_fn::<OrderPlaced, _, _>(move |evt| {
            let tx = Arc::clone(&tx);
            let evt = evt.clone();
            async move {
                if let Some(sender) = tx.lock().await.take() {
                    let _ = sender.send(evt);
                }
                Ok(())
            }
        });

        let handle = bus.bridge::<OrderPlaced>().await.unwrap();
        let received = tokio::time::timeout(Duration::from_secs(3), rx)
            .await
            .expect("bridge should reconnect and deliver event after transient transport errors")
            .expect("oneshot channel closed");
        assert_eq!(received, expected_event);
        assert!(pubsub.attempts.load(Ordering::SeqCst) >= 3);

        handle.shutdown().await;
    }
}
