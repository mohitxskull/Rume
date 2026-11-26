//! RabbitMQ [`MessageBroker`] implementation via `lapin`.

use std::fmt::Debug;

use crate::broker::{
    BoxFuture, BrokerConsumer, BrokerError, BrokerMessage, BrokerPublisher, MessageBroker,
};

/// RabbitMQ-backed [`MessageBroker`].
#[derive(Clone)]
pub struct RabbitMqBroker {
    connection: std::sync::Arc<lapin::Connection>,
}

impl Debug for RabbitMqBroker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RabbitMqBroker").finish_non_exhaustive()
    }
}

impl RabbitMqBroker {
    /// Wraps an existing `lapin::Connection`.
    pub fn new(connection: std::sync::Arc<lapin::Connection>) -> Self {
        Self { connection }
    }

    /// Connects to a RabbitMQ broker at `uri`.
    pub async fn connect(uri: &str) -> Result<Self, BrokerError> {
        let _ = uri;
        todo!("Task 06: implement RabbitMqBroker::connect")
    }

    /// Returns a reference to the underlying `lapin::Connection`.
    pub fn connection(&self) -> &lapin::Connection {
        &self.connection
    }
}

impl BrokerPublisher for RabbitMqBroker {
    fn publish(&self, _message: BrokerMessage) -> BoxFuture<'_, Result<(), BrokerError>> {
        todo!("Task 06: implement RabbitMqBroker::publish")
    }
}

impl MessageBroker for RabbitMqBroker {
    fn subscribe<'a>(
        &'a self,
        _topic: &'a str,
    ) -> BoxFuture<'a, Result<Box<dyn BrokerConsumer>, BrokerError>> {
        todo!("Task 06: implement RabbitMqBroker::subscribe")
    }
}
