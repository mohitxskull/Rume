//! Distributed tenant cache quota tracking.

use std::future::Future;
use std::pin::Pin;

use crate::cache::error::CacheError;

/// Pinned boxed future for quota operations.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Trait for tracking and enforcing tenant cache quotas across single or multiple instances.
pub trait TenantQuotaStore: Send + Sync + 'static {
    /// Attempts to record a new key for `tenant_id`.
    ///
    /// Returns `Ok(true)` if acquired, or `Ok(false)` if `max_keys` would be exceeded.
    fn acquire<'a>(
        &'a self,
        tenant_id: &'a str,
        max_keys: usize,
    ) -> BoxFuture<'a, Result<bool, CacheError>>;

    /// Decrements the active key count for `tenant_id`.
    fn release<'a>(&'a self, tenant_id: &'a str) -> BoxFuture<'a, Result<(), CacheError>>;

    /// Records a new key with optional TTL for `tenant_id`.
    fn record_key<'a>(
        &'a self,
        tenant_id: &'a str,
        key: &'a str,
        ttl: Option<std::time::Duration>,
        max_keys: usize,
    ) -> BoxFuture<'a, Result<bool, CacheError>> {
        let _ = (key, ttl);
        self.acquire(tenant_id, max_keys)
    }

    /// Removes a key from quota tracking for `tenant_id`.
    fn remove_key<'a>(
        &'a self,
        tenant_id: &'a str,
        _key: &'a str,
    ) -> BoxFuture<'a, Result<(), CacheError>> {
        self.release(tenant_id)
    }

    /// Returns the active cached key count for `tenant_id`.
    fn count<'a>(&'a self, tenant_id: &'a str) -> BoxFuture<'a, Result<usize, CacheError>>;

    /// Resets the cached key count for `tenant_id` to zero (e.g. after full tenant eviction).
    fn reset<'a>(&'a self, tenant_id: &'a str) -> BoxFuture<'a, Result<(), CacheError>>;
}

/// Thread-safe in-memory tenant quota store suitable for testing and single-instance deployments.
#[derive(Debug, Clone, Default)]
pub struct InMemoryTenantQuotaStore {
    counts: std::sync::Arc<tokio::sync::RwLock<std::collections::HashMap<String, usize>>>,
}

impl InMemoryTenantQuotaStore {
    /// Creates a new in-memory tenant quota store.
    pub fn new() -> Self {
        Self::default()
    }
}

impl TenantQuotaStore for InMemoryTenantQuotaStore {
    fn acquire<'a>(
        &'a self,
        tenant_id: &'a str,
        max_keys: usize,
    ) -> BoxFuture<'a, Result<bool, CacheError>> {
        let tid = tenant_id.to_string();
        Box::pin(async move {
            let mut map = self.counts.write().await;
            let count = map.entry(tid).or_insert(0);
            if *count >= max_keys {
                Ok(false)
            } else {
                *count += 1;
                Ok(true)
            }
        })
    }

    fn release<'a>(&'a self, tenant_id: &'a str) -> BoxFuture<'a, Result<(), CacheError>> {
        let tid = tenant_id.to_string();
        Box::pin(async move {
            let mut map = self.counts.write().await;
            if let Some(count) = map.get_mut(&tid) {
                *count = count.saturating_sub(1);
            }
            Ok(())
        })
    }

    fn count<'a>(&'a self, tenant_id: &'a str) -> BoxFuture<'a, Result<usize, CacheError>> {
        let tid = tenant_id.to_string();
        Box::pin(async move {
            let map = self.counts.read().await;
            Ok(map.get(&tid).copied().unwrap_or(0))
        })
    }

    fn reset<'a>(&'a self, tenant_id: &'a str) -> BoxFuture<'a, Result<(), CacheError>> {
        let tid = tenant_id.to_string();
        Box::pin(async move {
            let mut map = self.counts.write().await;
            map.remove(&tid);
            Ok(())
        })
    }
}

#[cfg(feature = "redis")]
pub use redis_store::RedisTenantQuotaStore;

#[cfg(feature = "redis")]
mod redis_store {
    use super::*;
    use std::fmt::Debug;
    use deadpool_redis::Pool;

    /// Production Redis-backed [`TenantQuotaStore`] coordinating tenant cache quotas across a cluster.
    #[derive(Clone)]
    pub struct RedisTenantQuotaStore {
        pool: Pool,
        prefix: String,
    }

    impl Debug for RedisTenantQuotaStore {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("RedisTenantQuotaStore")
                .field("prefix", &self.prefix)
                .finish()
        }
    }

    impl RedisTenantQuotaStore {
        /// Creates a new Redis tenant quota store.
        pub fn new(pool: Pool) -> Self {
            Self {
                pool,
                prefix: "rume:quota:".to_string(),
            }
        }

        /// Configures a custom key prefix.
        pub fn with_prefix(mut self, prefix: impl Into<String>) -> Self {
            self.prefix = prefix.into();
            self
        }

        fn key(&self, tenant_id: &str) -> String {
            format!("{}quota:{{{}}}:keys", self.prefix, tenant_id)
        }
    }

    impl TenantQuotaStore for RedisTenantQuotaStore {
        fn acquire<'a>(
            &'a self,
            tenant_id: &'a str,
            max_keys: usize,
        ) -> BoxFuture<'a, Result<bool, CacheError>> {
            let unique_id = format!("lease-{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos());
            Box::pin(async move {
                self.record_key(tenant_id, &unique_id, None, max_keys).await
            })
        }

        fn record_key<'a>(
            &'a self,
            tenant_id: &'a str,
            key: &'a str,
            ttl: Option<std::time::Duration>,
            max_keys: usize,
        ) -> BoxFuture<'a, Result<bool, CacheError>> {
            let k = self.key(tenant_id);
            let k_item = key.to_string();
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as i64;
            let expires_at_ms = match ttl {
                Some(t) => now_ms.saturating_add(t.as_millis() as i64),
                None => i64::MAX,
            };

            Box::pin(async move {
                let mut conn = self
                    .pool
                    .get()
                    .await
                    .map_err(|e| CacheError::Backend(e.to_string()))?;

                // 1. Prune expired keys from sorted set
                // 2. Count active keys
                // 3. If count >= max_keys, return 0
                // 4. Otherwise add/update key with expiration score and return 1
                let script = r#"
                    redis.call('ZREMRANGEBYSCORE', KEYS[1], '-inf', ARGV[1])
                    local count = redis.call('ZCARD', KEYS[1])
                    local exists = redis.call('ZSCORE', KEYS[1], ARGV[4])
                    if not exists and count >= tonumber(ARGV[2]) then
                        return 0
                    else
                        redis.call('ZADD', KEYS[1], ARGV[3], ARGV[4])
                        return 1
                    end
                "#;

                let res: i32 = redis::Script::new(script)
                    .key(&k)
                    .arg(now_ms)
                    .arg(max_keys)
                    .arg(expires_at_ms)
                    .arg(&k_item)
                    .invoke_async(&mut *conn)
                    .await
                    .map_err(|e| CacheError::Backend(e.to_string()))?;

                Ok(res == 1)
            })
        }

        fn remove_key<'a>(
            &'a self,
            tenant_id: &'a str,
            key: &'a str,
        ) -> BoxFuture<'a, Result<(), CacheError>> {
            let k = self.key(tenant_id);
            let k_item = key.to_string();
            Box::pin(async move {
                let mut conn = self
                    .pool
                    .get()
                    .await
                    .map_err(|e| CacheError::Backend(e.to_string()))?;

                redis::cmd("ZREM")
                    .arg(&k)
                    .arg(&k_item)
                    .query_async::<()>(&mut *conn)
                    .await
                    .map_err(|e| CacheError::Backend(e.to_string()))?;

                Ok(())
            })
        }

        fn release<'a>(&'a self, tenant_id: &'a str) -> BoxFuture<'a, Result<(), CacheError>> {
            let k = self.key(tenant_id);
            Box::pin(async move {
                let mut conn = self
                    .pool
                    .get()
                    .await
                    .map_err(|e| CacheError::Backend(e.to_string()))?;

                let script = r#"
                    local oldest = redis.call('ZRANGE', KEYS[1], 0, 0)
                    if oldest and #oldest > 0 then
                        redis.call('ZREM', KEYS[1], oldest[1])
                    end
                    return 1
                "#;

                redis::Script::new(script)
                    .key(&k)
                    .invoke_async::<()>(&mut *conn)
                    .await
                    .map_err(|e| CacheError::Backend(e.to_string()))?;

                Ok(())
            })
        }

        fn count<'a>(&'a self, tenant_id: &'a str) -> BoxFuture<'a, Result<usize, CacheError>> {
            let k = self.key(tenant_id);
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as i64;
            Box::pin(async move {
                let mut conn = self
                    .pool
                    .get()
                    .await
                    .map_err(|e| CacheError::Backend(e.to_string()))?;

                let script = r#"
                    redis.call('ZREMRANGEBYSCORE', KEYS[1], '-inf', ARGV[1])
                    return redis.call('ZCARD', KEYS[1])
                "#;

                let count: usize = redis::Script::new(script)
                    .key(&k)
                    .arg(now_ms)
                    .invoke_async(&mut *conn)
                    .await
                    .map_err(|e| CacheError::Backend(e.to_string()))?;

                Ok(count)
            })
        }

        fn reset<'a>(&'a self, tenant_id: &'a str) -> BoxFuture<'a, Result<(), CacheError>> {
            let k = self.key(tenant_id);
            Box::pin(async move {
                let mut conn = self
                    .pool
                    .get()
                    .await
                    .map_err(|e| CacheError::Backend(e.to_string()))?;

                redis::cmd("DEL")
                    .arg(&k)
                    .query_async::<()>(&mut *conn)
                    .await
                    .map_err(|e| CacheError::Backend(e.to_string()))?;

                Ok(())
            })
        }
    }
}
