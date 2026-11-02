//! Production Redis cache backend using deadpool-redis.
//!
//! Provides:
//! - Connection-pooled asynchronous Redis communication via deadpool-redis
//! - Key-value storage with optional millisecond TTL (`SET ... PX`)
//! - Redis Set-based tag invalidation (`SADD tag:<tag> <key>` + batch deletion)
//! - Namespace isolation and health ping checks

use std::fmt::Debug;
use std::time::Duration;

use deadpool_redis::Pool;

use crate::cache::backend::{BoxFuture, CacheBackend};
use crate::cache::error::CacheError;

/// Production Redis-backed implementation of [`CacheBackend`].
#[derive(Clone)]
pub struct RedisCacheBackend {
    pool: Pool,
    prefix: String,
}

impl Debug for RedisCacheBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RedisCacheBackend")
            .field("prefix", &self.prefix)
            .finish()
    }
}

impl RedisCacheBackend {
    /// Creates a new Redis cache backend from a connection URL (e.g. `"redis://127.0.0.1:6379"`).
    pub fn new(url: &str) -> Result<Self, CacheError> {
        let config = deadpool_redis::Config::from_url(url);
        let pool = config
            .create_pool(Some(deadpool_redis::Runtime::Tokio1))
            .map_err(|e| CacheError::Backend(format!("failed to create redis pool: {e}")))?;

        Ok(Self {
            pool,
            prefix: "rume:".to_string(),
        })
    }

    /// Creates a Redis cache backend from an existing `deadpool_redis::Pool`.
    pub fn from_pool(pool: Pool) -> Self {
        Self {
            pool,
            prefix: "rume:".to_string(),
        }
    }

    /// Configures a custom key prefix for cache namespace isolation.
    pub fn with_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.prefix = prefix.into();
        self
    }

    /// Returns a reference to the underlying connection pool.
    pub fn pool(&self) -> &Pool {
        &self.pool
    }

    fn prefixed_key(&self, key: &str) -> String {
        format!("{}{}", self.prefix, key)
    }

    fn tag_set_key(&self, tag: &str) -> String {
        format!("{}tag:{}", self.prefix, tag)
    }
}

impl CacheBackend for RedisCacheBackend {
    fn get(&self, key: &str) -> BoxFuture<'_, Result<Option<Vec<u8>>, CacheError>> {
        let k = self.prefixed_key(key);
        Box::pin(async move {
            let mut conn = self
                .pool
                .get()
                .await
                .map_err(|e| CacheError::Backend(e.to_string()))?;

            let val: Option<Vec<u8>> = redis::cmd("GET")
                .arg(&k)
                .query_async(&mut *conn)
                .await
                .map_err(|e| CacheError::Backend(e.to_string()))?;

            Ok(val)
        })
    }

    fn set(
        &self,
        key: &str,
        value: Vec<u8>,
        ttl: Option<Duration>,
    ) -> BoxFuture<'_, Result<(), CacheError>> {
        let k = self.prefixed_key(key);
        Box::pin(async move {
            let mut conn = self
                .pool
                .get()
                .await
                .map_err(|e| CacheError::Backend(e.to_string()))?;

            let mut cmd = redis::cmd("SET");
            cmd.arg(&k).arg(&value);
            if let Some(ttl) = ttl {
                cmd.arg("PX").arg(ttl.as_millis() as u64);
            }

            cmd.query_async::<()>(&mut *conn)
                .await
                .map_err(|e| CacheError::Backend(e.to_string()))?;

            Ok(())
        })
    }

    fn set_with_tags(
        &self,
        key: &str,
        value: Vec<u8>,
        tags: &[&str],
        ttl: Option<Duration>,
    ) -> BoxFuture<'_, Result<(), CacheError>> {
        let k = self.prefixed_key(key);
        let tag_keys: Vec<String> = tags.iter().map(|t| self.tag_set_key(t)).collect();
        Box::pin(async move {
            let mut conn = self
                .pool
                .get()
                .await
                .map_err(|e| CacheError::Backend(e.to_string()))?;

            let mut cmd = redis::cmd("SET");
            cmd.arg(&k).arg(&value);
            if let Some(ttl) = ttl {
                cmd.arg("PX").arg(ttl.as_millis() as u64);
            }

            cmd.query_async::<()>(&mut *conn)
                .await
                .map_err(|e| CacheError::Backend(e.to_string()))?;

            for tk in tag_keys {
                redis::cmd("SADD")
                    .arg(&tk)
                    .arg(&k)
                    .query_async::<()>(&mut *conn)
                    .await
                    .map_err(|e| CacheError::Backend(e.to_string()))?;
            }

            Ok(())
        })
    }

    fn delete(&self, key: &str) -> BoxFuture<'_, Result<bool, CacheError>> {
        let k = self.prefixed_key(key);
        Box::pin(async move {
            let mut conn = self
                .pool
                .get()
                .await
                .map_err(|e| CacheError::Backend(e.to_string()))?;

            let affected: u64 = redis::cmd("DEL")
                .arg(&k)
                .query_async(&mut *conn)
                .await
                .map_err(|e| CacheError::Backend(e.to_string()))?;

            Ok(affected > 0)
        })
    }

    fn exists(&self, key: &str) -> BoxFuture<'_, Result<bool, CacheError>> {
        let k = self.prefixed_key(key);
        Box::pin(async move {
            let mut conn = self
                .pool
                .get()
                .await
                .map_err(|e| CacheError::Backend(e.to_string()))?;

            let exists: bool = redis::cmd("EXISTS")
                .arg(&k)
                .query_async(&mut *conn)
                .await
                .map_err(|e| CacheError::Backend(e.to_string()))?;

            Ok(exists)
        })
    }

    fn invalidate_tag(&self, tag: &str) -> BoxFuture<'_, Result<usize, CacheError>> {
        let tk = self.tag_set_key(tag);
        Box::pin(async move {
            let mut conn = self
                .pool
                .get()
                .await
                .map_err(|e| CacheError::Backend(e.to_string()))?;

            let keys: Vec<String> = redis::cmd("SMEMBERS")
                .arg(&tk)
                .query_async(&mut *conn)
                .await
                .map_err(|e| CacheError::Backend(e.to_string()))?;

            let count = keys.len();
            if !keys.is_empty() {
                let mut del_cmd = redis::cmd("DEL");
                for k in &keys {
                    del_cmd.arg(k);
                }
                del_cmd.arg(&tk);
                del_cmd
                    .query_async::<()>(&mut *conn)
                    .await
                    .map_err(|e| CacheError::Backend(e.to_string()))?;
            } else {
                let _ = redis::cmd("DEL")
                    .arg(&tk)
                    .query_async::<()>(&mut *conn)
                    .await;
            }

            Ok(count)
        })
    }

    fn clear(&self) -> BoxFuture<'_, Result<(), CacheError>> {
        Box::pin(async move {
            let mut conn = self
                .pool
                .get()
                .await
                .map_err(|e| CacheError::Backend(e.to_string()))?;

            redis::cmd("FLUSHDB")
                .query_async::<()>(&mut *conn)
                .await
                .map_err(|e| CacheError::Backend(e.to_string()))?;

            Ok(())
        })
    }

    fn ping(&self) -> BoxFuture<'_, Result<(), CacheError>> {
        Box::pin(async move {
            let mut conn = self
                .pool
                .get()
                .await
                .map_err(|e| CacheError::Backend(e.to_string()))?;

            redis::cmd("PING")
                .query_async::<()>(&mut *conn)
                .await
                .map_err(|e| CacheError::Backend(e.to_string()))?;

            Ok(())
        })
    }
}
