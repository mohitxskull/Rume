//! PostgreSQL `LISTEN`/`NOTIFY` [`PubSub`] implementation.

use sqlx::PgPool;

use crate::pubsub::{BoxFuture, PubSub, PubSubError, PubSubSubscription};

/// PostgreSQL [`PubSub`] backend using `pg_notify` and `PgListener`.
#[derive(Debug, Clone)]
pub struct PostgresPubSub {
    pool: PgPool,
}

impl PostgresPubSub {
    /// Creates a new [`PostgresPubSub`] backed by `pool`.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Returns a reference to the underlying PostgreSQL pool.
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }
}

impl PubSub for PostgresPubSub {
    fn publish<'a>(
        &'a self,
        _topic: &'a str,
        _payload: &'a [u8],
    ) -> BoxFuture<'a, Result<(), PubSubError>> {
        todo!("Task 05: implement PostgresPubSub::publish")
    }

    fn subscribe<'a>(
        &'a self,
        _topic: &'a str,
    ) -> BoxFuture<'a, Result<Box<dyn PubSubSubscription>, PubSubError>> {
        todo!("Task 05: implement PostgresPubSub::subscribe")
    }
}
