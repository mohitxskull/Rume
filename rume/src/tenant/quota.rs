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

    /// Returns the active cached key count for `tenant_id`.
    fn count<'a>(&'a self, tenant_id: &'a str) -> BoxFuture<'a, Result<usize, CacheError>>;

    /// Resets the cached key count for `tenant_id` to zero (e.g. after full tenant eviction).
    fn reset<'a>(&'a self, tenant_id: &'a str) -> BoxFuture<'a, Result<(), CacheError>>;
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
            format!("{}{}", self.prefix, tenant_id)
        }
    }

    impl TenantQuotaStore for RedisTenantQuotaStore {
        fn acquire<'a>(
            &'a self,
            tenant_id: &'a str,
            max_keys: usize,
        ) -> BoxFuture<'a, Result<bool, CacheError>> {
            let k = self.key(tenant_id);
            Box::pin(async move {
                let mut conn = self
                    .pool
                    .get()
                    .await
                    .map_err(|e| CacheError::Backend(e.to_string()))?;

                // Atomic quota check and increment Lua script
                let script = r#"
                    local current = redis.call('GET', KEYS[1])
                    if current and tonumber(current) >= tonumber(ARGV[1]) then
                        return 0
                    else
                        redis.call('INCR', KEYS[1])
                        return 1
                    end
                "#;

                let res: i32 = redis::Script::new(script)
                    .key(&k)
                    .arg(max_keys)
                    .invoke_async(&mut *conn)
                    .await
                    .map_err(|e| CacheError::Backend(e.to_string()))?;

                Ok(res == 1)
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
                    local current = redis.call('GET', KEYS[1])
                    if current and tonumber(current) > 0 then
                        redis.call('DECR', KEYS[1])
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
            Box::pin(async move {
                let mut conn = self
                    .pool
                    .get()
                    .await
                    .map_err(|e| CacheError::Backend(e.to_string()))?;

                let val: Option<usize> = redis::cmd("GET")
                    .arg(&k)
                    .query_async(&mut *conn)
                    .await
                    .map_err(|e| CacheError::Backend(e.to_string()))?;

                Ok(val.unwrap_or(0))
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
