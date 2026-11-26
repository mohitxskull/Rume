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
    pub async fn publish<E: Event + Serialize>(&self, _event: &E) -> Result<(), EventError> {
        todo!("Task 05: implement DistributedEventBus::publish")
    }

    /// Subscribes to remote events of type `E` and dispatches them to the local [`EventBus`],
    /// ignoring echoes from `self.node_id`.
    pub async fn bridge<E: Event + for<'de> Deserialize<'de>>(&self) -> Result<(), EventError> {
        todo!("Task 05: implement DistributedEventBus::bridge")
    }
}
