//! Redis-backed [`PubSub`] implementation.

use std::fmt::Debug;

use deadpool_redis::Pool;
use futures_util::StreamExt;

use crate::pubsub::{BoxFuture, PubSub, PubSubError, PubSubMessage, PubSubSubscription};

/// Redis [`PubSub`] backend using `PUBLISH` and `SUBSCRIBE`.
#[derive(Clone)]
pub struct RedisPubSub {
    pool: Pool,
    // Why: `deadpool_redis::Manager` keeps its `redis::Client` private and hands out
    // `MultiplexedConnection`s, which cannot enter `SUBSCRIBE` mode; `from_url` / `with_client`
    // retain a `redis::Client` so `subscribe()` can open a dedicated `redis::aio::PubSub`.
    client: Option<redis::Client>,
}

impl Debug for RedisPubSub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RedisPubSub")
            .field("has_client", &self.client.is_some())
            .finish_non_exhaustive()
    }
}

impl RedisPubSub {
    /// Creates a new [`RedisPubSub`] from a `deadpool_redis::Pool` (supports `publish` only).
    pub fn new(pool: Pool) -> Self {
        Self { pool, client: None }
    }

    /// Creates a new [`RedisPubSub`] with both a `deadpool_redis::Pool` and a `redis::Client`
    /// for dedicated `SUBSCRIBE` connections.
    pub fn with_client(pool: Pool, client: redis::Client) -> Self {
        Self {
            pool,
            client: Some(client),
        }
    }

    /// Creates a new [`RedisPubSub`] from a Redis connection URL (supports both `publish` and `subscribe`).
    pub fn from_url(url: &str) -> Result<Self, PubSubError> {
        let config = deadpool_redis::Config::from_url(url);
        let pool = config
            .create_pool(Some(deadpool_redis::Runtime::Tokio1))
            .map_err(|e| PubSubError::Transport(format!("failed to create redis pool: {e}")))?;
        let client = redis::Client::open(url)
            .map_err(|e| PubSubError::Transport(format!("failed to open redis client: {e}")))?;
        Ok(Self {
            pool,
            client: Some(client),
        })
    }

    /// Returns a reference to the underlying Redis pool.
    pub fn pool(&self) -> &Pool {
        &self.pool
    }
}

struct RedisSubscription {
    stream: redis::aio::PubSubStream,
}

impl Debug for RedisSubscription {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RedisSubscription").finish_non_exhaustive()
    }
}

impl PubSubSubscription for RedisSubscription {
    fn recv(&mut self) -> BoxFuture<'_, Result<Option<PubSubMessage>, PubSubError>> {
        Box::pin(async move {
            match self.stream.next().await {
                Some(msg) => Ok(Some(PubSubMessage {
                    topic: msg.get_channel_name().to_string(),
                    payload: msg.get_payload_bytes().to_vec(),
                })),
                None => Ok(None),
            }
        })
    }
}

impl PubSub for RedisPubSub {
    fn publish<'a>(
        &'a self,
        topic: &'a str,
        payload: &'a [u8],
    ) -> BoxFuture<'a, Result<(), PubSubError>> {
        Box::pin(async move {
            let mut conn = self
                .pool
                .get()
                .await
                .map_err(|e| PubSubError::Transport(format!("redis pool get failed: {e}")))?;
            redis::cmd("PUBLISH")
                .arg(topic)
                .arg(payload)
                .query_async::<i64>(&mut *conn)
                .await
                .map_err(|e| PubSubError::Transport(format!("redis PUBLISH failed: {e}")))?;
            Ok(())
        })
    }

    fn subscribe<'a>(
        &'a self,
        topic: &'a str,
    ) -> BoxFuture<'a, Result<Box<dyn PubSubSubscription>, PubSubError>> {
        Box::pin(async move {
            let client = self.client.as_ref().ok_or_else(|| {
                PubSubError::Transport(
                    "RedisPubSub::subscribe requires a redis::Client; construct via RedisPubSub::from_url or RedisPubSub::with_client".into(),
                )
            })?;
            let mut pubsub = client.get_async_pubsub().await.map_err(|e| {
                PubSubError::Transport(format!("redis get_async_pubsub failed: {e}"))
            })?;
            pubsub
                .subscribe(topic)
                .await
                .map_err(|e| PubSubError::Transport(format!("redis SUBSCRIBE failed: {e}")))?;
            Ok(Box::new(RedisSubscription {
                stream: pubsub.into_on_message(),
            }) as Box<dyn PubSubSubscription>)
        })
    }
}
