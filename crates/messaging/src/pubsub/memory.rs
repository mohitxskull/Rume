//! In-memory [`PubSub`] backend backed by `tokio::sync::broadcast`.

use crate::pubsub::{BoxFuture, PubSub, PubSubError, PubSubSubscription};

/// In-memory [`PubSub`] implementation for hermetic tests and single-node deployments.
#[derive(Debug, Default)]
pub struct InMemoryPubSub;

impl InMemoryPubSub {
    /// Creates a new in-memory pub/sub hub.
    pub fn new() -> Self {
        Self
    }
}

impl PubSub for InMemoryPubSub {
    fn publish<'a>(
        &'a self,
        _topic: &'a str,
        _payload: &'a [u8],
    ) -> BoxFuture<'a, Result<(), PubSubError>> {
        todo!("Task 05: implement InMemoryPubSub::publish")
    }

    fn subscribe<'a>(
        &'a self,
        _topic: &'a str,
    ) -> BoxFuture<'a, Result<Box<dyn PubSubSubscription>, PubSubError>> {
        todo!("Task 05: implement InMemoryPubSub::subscribe")
    }
}
