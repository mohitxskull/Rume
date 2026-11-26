//! In-memory [`MessageBroker`] for hermetic tests and local development.

use crate::broker::{
    BoxFuture, BrokerConsumer, BrokerError, BrokerMessage, BrokerPublisher, MessageBroker,
};

/// In-memory [`MessageBroker`] implementation.
#[derive(Debug, Default)]
pub struct InMemoryBroker;

impl InMemoryBroker {
    /// Creates a new in-memory message broker.
    pub fn new() -> Self {
        Self
    }
}

impl BrokerPublisher for InMemoryBroker {
    fn publish(&self, _message: BrokerMessage) -> BoxFuture<'_, Result<(), BrokerError>> {
        todo!("Task 06: implement InMemoryBroker::publish")
    }
}

impl MessageBroker for InMemoryBroker {
    fn subscribe<'a>(
        &'a self,
        _topic: &'a str,
    ) -> BoxFuture<'a, Result<Box<dyn BrokerConsumer>, BrokerError>> {
        todo!("Task 06: implement InMemoryBroker::subscribe")
    }
}
