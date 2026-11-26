//! Redis-backed [`PubSub`] implementation.

use std::fmt::Debug;

use deadpool_redis::Pool;

use crate::pubsub::{BoxFuture, PubSub, PubSubError, PubSubSubscription};

/// Redis [`PubSub`] backend using `PUBLISH` and `SUBSCRIBE`.
#[derive(Clone)]
pub struct RedisPubSub {
    pool: Pool,
}

impl Debug for RedisPubSub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RedisPubSub").finish_non_exhaustive()
    }
}

impl RedisPubSub {
    /// Creates a new [`RedisPubSub`] from a `deadpool_redis::Pool`.
    pub fn new(pool: Pool) -> Self {
        Self { pool }
    }

    /// Returns a reference to the underlying Redis pool.
    pub fn pool(&self) -> &Pool {
        &self.pool
    }
}

impl PubSub for RedisPubSub {
    fn publish<'a>(
        &'a self,
        _topic: &'a str,
        _payload: &'a [u8],
    ) -> BoxFuture<'a, Result<(), PubSubError>> {
        todo!("Task 05: implement RedisPubSub::publish")
    }

    fn subscribe<'a>(
        &'a self,
        _topic: &'a str,
    ) -> BoxFuture<'a, Result<Box<dyn PubSubSubscription>, PubSubError>> {
        todo!("Task 05: implement RedisPubSub::subscribe")
    }
}
