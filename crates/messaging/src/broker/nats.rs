//! NATS [`MessageBroker`] implementation via `async-nats`.

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
        let _ = url;
        todo!("Task 06: implement NatsBroker::connect")
    }

    /// Returns a reference to the underlying `async_nats::Client`.
    pub fn client(&self) -> &async_nats::Client {
        &self.client
    }
}

impl BrokerPublisher for NatsBroker {
    fn publish(&self, _message: BrokerMessage) -> BoxFuture<'_, Result<(), BrokerError>> {
        todo!("Task 06: implement NatsBroker::publish")
    }
}

impl MessageBroker for NatsBroker {
    fn subscribe<'a>(
        &'a self,
        _topic: &'a str,
    ) -> BoxFuture<'a, Result<Box<dyn BrokerConsumer>, BrokerError>> {
        todo!("Task 06: implement NatsBroker::subscribe")
    }
}
