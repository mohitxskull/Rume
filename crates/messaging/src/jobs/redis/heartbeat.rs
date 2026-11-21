use std::fmt::Debug;
use std::time::Duration;

use deadpool_redis::Pool;

use crate::jobs::error::JobError;
use crate::jobs::job::BoxFuture;
use crate::jobs::reliability::HeartbeatRegistry;
use rume_data::cache::redis::RedisCacheBackend;

/// Redis-backed implementation of [`HeartbeatRegistry`].
///
/// Uses Redis Sorted Sets (`ZADD`, `ZSCORE`, `ZRANGEBYSCORE`) to track active worker nodes
/// across a cluster and reliably detect/reap deceased workers.
#[derive(Clone)]
pub struct RedisHeartbeatRegistry {
    pool: Pool,
    set_key: String,
}

impl Debug for RedisHeartbeatRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RedisHeartbeatRegistry")
            .field("set_key", &self.set_key)
            .finish()
    }
}

impl RedisHeartbeatRegistry {
    /// Creates a new heartbeat registry from a `deadpool_redis::Pool`.
    pub fn new(pool: Pool) -> Self {
        Self {
            pool,
            set_key: "rume:jobs:heartbeats".to_string(),
        }
    }

    /// Creates a heartbeat registry from a [`RedisCacheBackend`].
    pub fn from_backend(backend: &RedisCacheBackend) -> Self {
        Self {
            pool: backend.pool().clone(),
            set_key: "rume:jobs:heartbeats".to_string(),
        }
    }

    /// Configures a custom key for heartbeat tracking.
    pub fn with_set_key(mut self, key: impl Into<String>) -> Self {
        self.set_key = key.into();
        self
    }
}

impl HeartbeatRegistry for RedisHeartbeatRegistry {
    fn record_heartbeat(
        &self,
        worker_id: &str,
        ttl: Duration,
    ) -> BoxFuture<'_, Result<(), JobError>> {
        let wid = worker_id.to_string();
        let ttl_ms = ttl.as_millis() as i64;
        Box::pin(async move {
            let mut conn = self
                .pool
                .get()
                .await
                .map_err(|e| JobError::Fatal(e.to_string()))?;

            // Compute expires_at using Redis server TIME to eliminate host clock skew
            let script = redis::Script::new(
                r#"
                if redis.replicate_commands then redis.replicate_commands() end
                local server_time = redis.call('TIME')
                local now_ms = tonumber(server_time[1]) * 1000 + math.floor(tonumber(server_time[2]) / 1000)
                local expires_at = now_ms + tonumber(ARGV[2])
                redis.call('ZADD', KEYS[1], expires_at, ARGV[1])
                return 1
                "#,
            );

            let _: () = script
                .key(&self.set_key)
                .arg(&wid)
                .arg(ttl_ms)
                .invoke_async(&mut *conn)
                .await
                .map_err(|e| JobError::Fatal(e.to_string()))?;

            Ok(())
        })
    }

    fn is_alive(&self, worker_id: &str) -> BoxFuture<'_, Result<bool, JobError>> {
        let wid = worker_id.to_string();
        Box::pin(async move {
            let mut conn = self
                .pool
                .get()
                .await
                .map_err(|e| JobError::Fatal(e.to_string()))?;

            // Compare score against Redis server TIME to eliminate host clock skew
            let script = redis::Script::new(
                r#"
                local score = redis.call('ZSCORE', KEYS[1], ARGV[1])
                if not score then return 0 end
                local server_time = redis.call('TIME')
                local now_ms = tonumber(server_time[1]) * 1000 + math.floor(tonumber(server_time[2]) / 1000)
                if tonumber(score) > now_ms then return 1 else return 0 end
                "#,
            );

            let alive: i64 = script
                .key(&self.set_key)
                .arg(&wid)
                .invoke_async(&mut *conn)
                .await
                .map_err(|e| JobError::Fatal(e.to_string()))?;

            Ok(alive == 1)
        })
    }

    fn reap_dead_workers(&self) -> BoxFuture<'_, Result<Vec<String>, JobError>> {
        Box::pin(async move {
            let mut conn = self
                .pool
                .get()
                .await
                .map_err(|e| JobError::Fatal(e.to_string()))?;

            // Query dead worker IDs with score <= Redis server TIME, remove them atomically, and return list
            let script = redis::Script::new(
                r#"
                if redis.replicate_commands then redis.replicate_commands() end
                local server_time = redis.call('TIME')
                local now_ms = tonumber(server_time[1]) * 1000 + math.floor(tonumber(server_time[2]) / 1000)
                local dead = redis.call('ZRANGEBYSCORE', KEYS[1], '-inf', now_ms)
                if #dead > 0 then
                    redis.call('ZREMRANGEBYSCORE', KEYS[1], '-inf', now_ms)
                end
                return dead
                "#,
            );

            let dead_workers: Vec<String> = script
                .key(&self.set_key)
                .invoke_async(&mut *conn)
                .await
                .map_err(|e| JobError::Fatal(e.to_string()))?;

            Ok(dead_workers)
        })
    }
}
