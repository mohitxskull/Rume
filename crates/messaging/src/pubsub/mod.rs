//! Distributed publish/subscribe abstraction and backends.

pub mod memory;
#[cfg(feature = "postgres")]
pub mod postgres;
#[cfg(feature = "redis")]
pub mod redis;

use std::fmt::Debug;
use std::future::Future;
use std::pin::Pin;

use thiserror::Error;

pub use memory::InMemoryPubSub;
#[cfg(feature = "postgres")]
pub use postgres::PostgresPubSub;
#[cfg(feature = "redis")]
pub use redis::RedisPubSub;

/// Pinned boxed future type alias for async pub/sub operations.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Message delivered over a [`PubSub`] topic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PubSubMessage {
    /// Topic or channel name.
    pub topic: String,
    /// Raw payload bytes.
    pub payload: Vec<u8>,
}

/// Errors returned by [`PubSub`] backends and subscriptions.
#[derive(Debug, Error)]
pub enum PubSubError {
    /// Backend connection or transport failure.
    #[error("pubsub transport error: {0}")]
    Transport(String),
    /// Subscription closed or lagged.
    #[error("pubsub subscription closed: {0}")]
    Closed(String),
}

/// Active subscription handle receiving messages for a topic.
pub trait PubSubSubscription: Debug + Send + Sync {
    /// Receives the next message from the subscribed topic, or `None` when closed.
    fn recv(&mut self) -> BoxFuture<'_, Result<Option<PubSubMessage>, PubSubError>>;
}

/// Distributed publish/subscribe transport contract.
pub trait PubSub: Debug + Send + Sync + 'static {
    /// Publishes a raw payload to `topic`.
    fn publish<'a>(
        &'a self,
        topic: &'a str,
        payload: &'a [u8],
    ) -> BoxFuture<'a, Result<(), PubSubError>>;

    /// Subscribes to `topic`, returning a [`PubSubSubscription`] stream.
    fn subscribe<'a>(
        &'a self,
        topic: &'a str,
    ) -> BoxFuture<'a, Result<Box<dyn PubSubSubscription>, PubSubError>>;
}
