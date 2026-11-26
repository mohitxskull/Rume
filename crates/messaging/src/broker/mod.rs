//! External message broker abstractions (`InMemoryBroker`, `NatsBroker`, `RabbitMqBroker`).

pub mod memory;
#[cfg(feature = "nats")]
pub mod nats;
#[cfg(feature = "rabbitmq")]
pub mod rabbitmq;

use std::collections::BTreeMap;
use std::fmt::Debug;
use std::future::Future;
use std::pin::Pin;

use thiserror::Error;

pub use memory::InMemoryBroker;
#[cfg(feature = "nats")]
pub use nats::NatsBroker;
#[cfg(feature = "rabbitmq")]
pub use rabbitmq::RabbitMqBroker;

/// Pinned boxed future type alias for async broker operations.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Message published to or consumed from a [`MessageBroker`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrokerMessage {
    /// Destination topic, subject, or routing key.
    pub topic: String,
    /// Raw payload bytes.
    pub payload: Vec<u8>,
    /// Optional metadata headers (e.g. W3C `traceparent`, `event_type`).
    pub headers: BTreeMap<String, String>,
}

/// Errors returned by [`MessageBroker`], [`BrokerPublisher`], and [`BrokerConsumer`].
#[derive(Debug, Error)]
pub enum BrokerError {
    /// Broker connection or transport failure.
    #[error("broker transport error: {0}")]
    Transport(String),
    /// Consumer stream closed.
    #[error("broker consumer closed: {0}")]
    Closed(String),
}

/// Consumer handle yielding messages from a broker subscription.
pub trait BrokerConsumer: Debug + Send + Sync {
    /// Receives the next message from the subscribed topic/subject.
    fn next_message(&mut self) -> BoxFuture<'_, Result<Option<BrokerMessage>, BrokerError>>;
}

/// Publishing contract for external message brokers.
pub trait BrokerPublisher: Debug + Send + Sync + 'static {
    /// Publishes a [`BrokerMessage`] to its target topic/subject.
    fn publish(&self, message: BrokerMessage) -> BoxFuture<'_, Result<(), BrokerError>>;
}

/// Full message broker contract supporting both publishing and consuming.
pub trait MessageBroker: BrokerPublisher {
    /// Subscribes to `topic`, returning a [`BrokerConsumer`].
    fn subscribe<'a>(
        &'a self,
        topic: &'a str,
    ) -> BoxFuture<'a, Result<Box<dyn BrokerConsumer>, BrokerError>>;
}
