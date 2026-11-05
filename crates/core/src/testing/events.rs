//! In-memory event bus test double with assertion harness.

use std::any::{Any, TypeId};
use std::fmt::Debug;
use std::sync::{Arc, RwLock};

use crate::events::{Event, EventBus, EventHandler};

/// Representation of a captured event recorded by [`TestEventBus`].
pub type RecordedEvent = (TypeId, &'static str, Arc<dyn Any + Send + Sync>);

/// An in-memory test double for [`EventBus`] that records all published events for assertions.
#[derive(Clone, Default)]
pub struct TestEventBus {
    inner: EventBus,
    recorded: Arc<RwLock<Vec<RecordedEvent>>>,
}

impl Debug for TestEventBus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let count = self.recorded.read().map(|r| r.len()).unwrap_or(0);
        f.debug_struct("TestEventBus")
            .field("recorded_events_count", &count)
            .finish()
    }
}

impl TestEventBus {
    /// Creates a new empty test event bus.
    pub fn new() -> Self {
        Self::default()
    }

    /// Access the underlying [`EventBus`].
    pub fn bus(&self) -> &EventBus {
        &self.inner
    }

    /// Subscribes a handler to an event type.
    pub fn subscribe<E: Event, H: EventHandler<E>>(&self, handler: H) {
        self.inner.subscribe(handler);
    }

    /// Dispatches an event synchronously, recording it in the test log.
    pub async fn publish_sync<E: Event>(
        &self,
        event: &E,
    ) -> Result<usize, crate::events::EventError> {
        let count = self.inner.publish_sync(event).await?;
        if let Ok(mut recorded) = self.recorded.write() {
            recorded.push((TypeId::of::<E>(), E::TYPE, Arc::new(event.clone())));
        }
        Ok(count)
    }

    /// Dispatches an event asynchronously, recording it in the test log.
    pub fn publish_async<E: Event>(&self, event: E) -> usize {
        if let Ok(mut recorded) = self.recorded.write() {
            recorded.push((TypeId::of::<E>(), E::TYPE, Arc::new(event.clone())));
        }
        self.inner.publish_async(event)
    }

    /// Asserts that at least one event of type `E` was published, returning all published instances.
    pub fn assert_published<E: Event>(&self) -> Vec<E> {
        let events = self.published::<E>();
        assert!(
            !events.is_empty(),
            "expected at least one '{}' event to be published, but none were recorded",
            E::TYPE
        );
        events
    }

    /// Asserts that exactly `expected` events of type `E` were published.
    pub fn assert_published_count<E: Event>(&self, expected: usize) {
        let count = self.published::<E>().len();
        assert_eq!(
            count,
            expected,
            "expected {} '{}' events, but found {}",
            expected,
            E::TYPE,
            count
        );
    }

    /// Returns whether any events of type `E` have been published.
    pub fn has_published<E: Event>(&self) -> bool {
        !self.published::<E>().is_empty()
    }

    /// Retrieves all published events of type `E`.
    pub fn published<E: Event>(&self) -> Vec<E> {
        if let Ok(recorded) = self.recorded.read() {
            recorded
                .iter()
                .filter(|(tid, _, _)| *tid == TypeId::of::<E>())
                .filter_map(|(_, _, arc)| arc.downcast_ref::<E>().cloned())
                .collect()
        } else {
            Vec::new()
        }
    }

    /// Clears all recorded events from the test log.
    pub fn clear(&self) {
        if let Ok(mut recorded) = self.recorded.write() {
            recorded.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    struct OrderCreated {
        order_id: String,
        amount: u64,
    }

    impl Event for OrderCreated {
        const TYPE: &'static str = "orders.created";
    }

    #[tokio::test]
    async fn test_event_bus_assert_published() {
        let bus = TestEventBus::new();
        bus.publish_sync(&OrderCreated {
            order_id: "order-1".into(),
            amount: 100,
        })
        .await
        .unwrap();

        assert!(bus.has_published::<OrderCreated>());
        bus.assert_published_count::<OrderCreated>(1);
        let events = bus.assert_published::<OrderCreated>();
        assert_eq!(events[0].order_id, "order-1");

        bus.clear();
        assert!(!bus.has_published::<OrderCreated>());
    }
}
