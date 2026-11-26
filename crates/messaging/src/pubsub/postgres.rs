//! PostgreSQL `LISTEN`/`NOTIFY` [`PubSub`] implementation.

use sqlx::PgPool;
use sqlx::postgres::PgListener;

use crate::pubsub::{BoxFuture, PubSub, PubSubError, PubSubMessage, PubSubSubscription};

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

#[derive(Debug)]
struct PostgresSubscription {
    listener: PgListener,
}

impl PubSubSubscription for PostgresSubscription {
    fn recv(&mut self) -> BoxFuture<'_, Result<Option<PubSubMessage>, PubSubError>> {
        Box::pin(async move {
            match self.listener.recv().await {
                Ok(notification) => Ok(Some(PubSubMessage {
                    topic: notification.channel().to_string(),
                    payload: notification.payload().as_bytes().to_vec(),
                })),
                Err(e) => Err(PubSubError::Transport(format!(
                    "postgres PgListener recv failed: {e}"
                ))),
            }
        })
    }
}

impl PubSub for PostgresPubSub {
    fn publish<'a>(
        &'a self,
        topic: &'a str,
        payload: &'a [u8],
    ) -> BoxFuture<'a, Result<(), PubSubError>> {
        Box::pin(async move {
            let payload_str = std::str::from_utf8(payload).map_err(|e| {
                PubSubError::Transport(format!(
                    "postgres pg_notify payload must be valid UTF-8: {e}"
                ))
            })?;
            sqlx::query("SELECT pg_notify($1, $2)")
                .bind(topic)
                .bind(payload_str)
                .execute(&self.pool)
                .await
                .map_err(|e| PubSubError::Transport(format!("postgres pg_notify failed: {e}")))?;
            Ok(())
        })
    }

    fn subscribe<'a>(
        &'a self,
        topic: &'a str,
    ) -> BoxFuture<'a, Result<Box<dyn PubSubSubscription>, PubSubError>> {
        Box::pin(async move {
            let mut listener = PgListener::connect_with(&self.pool).await.map_err(|e| {
                PubSubError::Transport(format!("postgres PgListener connect failed: {e}"))
            })?;
            listener
                .listen(topic)
                .await
                .map_err(|e| PubSubError::Transport(format!("postgres LISTEN failed: {e}")))?;
            Ok(Box::new(PostgresSubscription { listener }) as Box<dyn PubSubSubscription>)
        })
    }
}
