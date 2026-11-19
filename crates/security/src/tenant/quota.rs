//! Distributed tenant cache quota tracking.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use rume_data::cache::error::CacheError;

/// Pinned boxed future for quota operations.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Opaque token identifying a single acquired quota lease.
///
/// Passing this token to [`TenantQuotaStore::release_lease`] ensures a late
/// completion only releases its own lease and never evicts a replacement lease.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LeaseId(String);

impl LeaseId {
    /// Creates a new unique lease identifier.
    pub fn new() -> Self {
        Self(format!("lease-{}", uuid::Uuid::new_v4()))
    }

    /// Returns the underlying string representation of the lease identifier.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for LeaseId {
    fn default() -> Self {
        Self::new()
    }
}

fn validate_ttl(ttl: Option<Duration>) -> Result<(), CacheError> {
    if let Some(d) = ttl
        && d.is_zero()
    {
        return Err(CacheError::Backend(
            "quota window/TTL must be greater than zero".into(),
        ));
    }
    Ok(())
}

/// Trait for tracking and enforcing tenant cache quotas across single or multiple instances.
pub trait TenantQuotaStore: Send + Sync + 'static {
    /// Attempts to record a new key for `tenant_id`.
    ///
    /// Returns `Ok(true)` if acquired, or `Ok(false)` if `max_keys` would be exceeded.
    fn acquire<'a>(
        &'a self,
        tenant_id: &'a str,
        max_keys: usize,
        window: Duration,
    ) -> BoxFuture<'a, Result<bool, CacheError>>;

    /// Decrements the active key count for `tenant_id`.
    fn release<'a>(&'a self, tenant_id: &'a str) -> BoxFuture<'a, Result<(), CacheError>>;

    /// Attempts to acquire a tracked lease for `tenant_id` valid for `window`.
    ///
    /// Returns `Ok(Some(lease_id))` if acquired, or `Ok(None)` if `max_keys` would be exceeded.
    fn acquire_lease<'a>(
        &'a self,
        tenant_id: &'a str,
        max_keys: usize,
        window: Duration,
    ) -> BoxFuture<'a, Result<Option<LeaseId>, CacheError>> {
        let lease_id = LeaseId::new();
        Box::pin(async move {
            if self
                .record_key(tenant_id, lease_id.as_str(), Some(window), max_keys)
                .await?
            {
                Ok(Some(lease_id))
            } else {
                Ok(None)
            }
        })
    }

    /// Releases a specific `lease_id` previously returned by [`TenantQuotaStore::acquire_lease`].
    ///
    /// If the lease already expired or was evicted, this is a no-op and never affects
    /// other active leases for `tenant_id`.
    fn release_lease<'a>(
        &'a self,
        tenant_id: &'a str,
        lease_id: &'a LeaseId,
    ) -> BoxFuture<'a, Result<(), CacheError>> {
        self.remove_key(tenant_id, lease_id.as_str())
    }

    /// Records a new key with optional TTL for `tenant_id`.
    ///
    /// # Contract Invariants (CRITICAL — MANDATORY METHOD):
    /// This method is intentionally **NOT defaulted** to delegate to `self.acquire()`.
    /// In multi-tenant cache management, writes to an *already existing* key (updates or TTL refreshes)
    /// must be idempotent and NEVER consume additional quota slots. If an implementation delegates
    /// `record_key` to a blind counter increment (`acquire`), cache overwrite loops will rapidly
    /// exhaust tenant quotas and cause spurious `QuotaExceeded` rejections.
    ///
    /// Implementations MUST:
    /// 1. Exclude or prune expired keys before evaluating `max_keys`.
    /// 2. Deduplicate by `key`: if `key` already exists for `tenant_id`, update its TTL without incrementing count.
    /// 3. Return `Ok(false)` only when `key` is new and active unexpired keys >= `max_keys`.
    fn record_key<'a>(
        &'a self,
        tenant_id: &'a str,
        key: &'a str,
        ttl: Option<std::time::Duration>,
        max_keys: usize,
    ) -> BoxFuture<'a, Result<bool, CacheError>>;

    /// Removes a key from quota tracking for `tenant_id`.
    ///
    /// Implementations MUST remove the specific `key` and only decrement quota if that key was
    /// actively present and tracked.
    fn remove_key<'a>(
        &'a self,
        tenant_id: &'a str,
        key: &'a str,
    ) -> BoxFuture<'a, Result<(), CacheError>>;

    /// Returns the active cached key count for `tenant_id`.
    fn count<'a>(&'a self, tenant_id: &'a str) -> BoxFuture<'a, Result<usize, CacheError>>;

    /// Resets the cached key count for `tenant_id` to zero (e.g. after full tenant eviction).
    fn reset<'a>(&'a self, tenant_id: &'a str) -> BoxFuture<'a, Result<(), CacheError>>;
}

type TenantKeyStore = std::collections::HashMap<String, Option<tokio::time::Instant>>;
type TenantKeyRegistry =
    std::sync::Arc<tokio::sync::RwLock<std::collections::HashMap<String, TenantKeyStore>>>;

/// Thread-safe in-memory tenant quota store suitable for testing and single-instance deployments.
#[derive(Debug, Clone, Default)]
pub struct InMemoryTenantQuotaStore {
    keys: TenantKeyRegistry,
}

impl InMemoryTenantQuotaStore {
    /// Creates a new in-memory tenant quota store.
    pub fn new() -> Self {
        Self::default()
    }

    fn prune_expired_inline(tenant_keys: &mut TenantKeyStore, now: tokio::time::Instant) {
        tenant_keys.retain(|_, expires_at| match expires_at {
            Some(exp) => *exp > now,
            None => true,
        });
    }
}

impl TenantQuotaStore for InMemoryTenantQuotaStore {
    fn acquire<'a>(
        &'a self,
        tenant_id: &'a str,
        max_keys: usize,
        window: Duration,
    ) -> BoxFuture<'a, Result<bool, CacheError>> {
        let unique_id = format!("lease-{}", uuid::Uuid::new_v4());
        Box::pin(async move {
            self.record_key(tenant_id, &unique_id, Some(window), max_keys)
                .await
        })
    }

    fn release<'a>(&'a self, tenant_id: &'a str) -> BoxFuture<'a, Result<(), CacheError>> {
        let tid = tenant_id.to_string();
        Box::pin(async move {
            let mut map = self.keys.write().await;
            let now = tokio::time::Instant::now();
            if let Some(tenant_map) = map.get_mut(&tid) {
                Self::prune_expired_inline(tenant_map, now);
                if let Some(first_key) = tenant_map.keys().next().cloned() {
                    tenant_map.remove(&first_key);
                }
            }
            Ok(())
        })
    }

    fn record_key<'a>(
        &'a self,
        tenant_id: &'a str,
        key: &'a str,
        ttl: Option<std::time::Duration>,
        max_keys: usize,
    ) -> BoxFuture<'a, Result<bool, CacheError>> {
        let tid = tenant_id.to_string();
        let k = key.to_string();
        Box::pin(async move {
            validate_ttl(ttl)?;
            let mut map = self.keys.write().await;
            let now = tokio::time::Instant::now();
            let expires_at = ttl.map(|d| now + d);
            let tenant_map = map.entry(tid).or_default();
            Self::prune_expired_inline(tenant_map, now);

            if tenant_map.contains_key(&k) {
                // Key already exists for this tenant: repeated writes/updates refresh TTL without consuming quota
                tenant_map.insert(k, expires_at);
                Ok(true)
            } else if tenant_map.len() >= max_keys {
                Ok(false)
            } else {
                tenant_map.insert(k, expires_at);
                Ok(true)
            }
        })
    }

    fn remove_key<'a>(
        &'a self,
        tenant_id: &'a str,
        key: &'a str,
    ) -> BoxFuture<'a, Result<(), CacheError>> {
        let tid = tenant_id.to_string();
        let k = key.to_string();
        Box::pin(async move {
            let mut map = self.keys.write().await;
            let now = tokio::time::Instant::now();
            if let Some(tenant_map) = map.get_mut(&tid) {
                Self::prune_expired_inline(tenant_map, now);
                tenant_map.remove(&k);
            }
            Ok(())
        })
    }

    fn count<'a>(&'a self, tenant_id: &'a str) -> BoxFuture<'a, Result<usize, CacheError>> {
        let tid = tenant_id.to_string();
        Box::pin(async move {
            let map = self.keys.read().await;
            let now = tokio::time::Instant::now();
            if let Some(tenant_map) = map.get(&tid) {
                let active = tenant_map
                    .values()
                    .filter(|exp| exp.is_none_or(|t| t > now))
                    .count();
                Ok(active)
            } else {
                Ok(0)
            }
        })
    }

    fn reset<'a>(&'a self, tenant_id: &'a str) -> BoxFuture<'a, Result<(), CacheError>> {
        let tid = tenant_id.to_string();
        Box::pin(async move {
            let mut keys = self.keys.write().await;
            keys.remove(&tid);
            Ok(())
        })
    }
}

#[cfg(feature = "redis")]
pub use redis_store::RedisTenantQuotaStore;

#[cfg(feature = "redis")]
mod redis_store {
    use super::*;
    use deadpool_redis::Pool;
    use std::fmt::Debug;

    /// Production Redis-backed [`TenantQuotaStore`] coordinating tenant cache quotas across a cluster.
    #[derive(Clone)]
    pub struct RedisTenantQuotaStore {
        pool: Pool,
        prefix: String,
    }

    // Why: A tenant's sorted set holds entries with heterogeneous (or unbounded) expirations.
    // Blindly calling `PEXPIRE KEYS[1] ttl_ms` on every `ZADD` would cause a short-lived key to
    // wipe out the whole sorted set before longer-lived keys expire (or expire `+inf` entries).
    // Instead, inspect the highest-scoring member via `ZREVRANGE ... WITHSCORES` and `PERSIST`
    // when any member is unbounded (`ttl_ms < 0` or `+inf`/`inf`), or `PEXPIRE` up to the
    // furthest member's remaining milliseconds.
    pub(super) const REDIS_RECORD_KEY_SCRIPT: &str = r#"
        if redis.replicate_commands then redis.replicate_commands() end
        local t = redis.call('TIME')
        local now_ms = tonumber(t[1]) * 1000 + math.floor(tonumber(t[2]) / 1000)
        redis.call('ZREMRANGEBYSCORE', KEYS[1], '-inf', now_ms)
        local count = redis.call('ZCARD', KEYS[1])
        local exists = redis.call('ZSCORE', KEYS[1], ARGV[3])
        if not exists and count >= tonumber(ARGV[2]) then
            return 0
        else
            local ttl_ms = tonumber(ARGV[1])
            local score
            if ttl_ms < 0 then
                score = '+inf'
            else
                score = now_ms + ttl_ms
            end
            redis.call('ZADD', KEYS[1], score, ARGV[3])
            local top = redis.call('ZREVRANGE', KEYS[1], 0, 0, 'WITHSCORES')
            local max_score_str = top[2]
            local max_score = max_score_str and tonumber(max_score_str) or nil
            if ttl_ms < 0 or max_score_str == 'inf' or max_score_str == '+inf' or not max_score or max_score == math.huge then
                redis.call('PERSIST', KEYS[1])
            else
                redis.call('PEXPIRE', KEYS[1], math.max(1, math.ceil(max_score - now_ms)))
            end
            return 1
        end
    "#;

    pub(super) const REDIS_COUNT_SCRIPT: &str = r#"
        if redis.replicate_commands then redis.replicate_commands() end
        local t = redis.call('TIME')
        local now_ms = tonumber(t[1]) * 1000 + math.floor(tonumber(t[2]) / 1000)
        redis.call('ZREMRANGEBYSCORE', KEYS[1], '-inf', now_ms)
        return redis.call('ZCARD', KEYS[1])
    "#;

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

        pub(super) fn key(&self, tenant_id: &str) -> String {
            format!("{}{{{}}}:keys", self.prefix, tenant_id)
        }
    }

    impl TenantQuotaStore for RedisTenantQuotaStore {
        fn acquire<'a>(
            &'a self,
            tenant_id: &'a str,
            max_keys: usize,
            window: Duration,
        ) -> BoxFuture<'a, Result<bool, CacheError>> {
            // Use UUID instead of SystemTime nanos to avoid lease-id collisions when concurrent
            // requests share the same nanosecond across multi-instance nodes.
            let unique_id = format!("lease-{}", uuid::Uuid::new_v4());
            Box::pin(async move {
                self.record_key(tenant_id, &unique_id, Some(window), max_keys)
                    .await
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

            Box::pin(async move {
                validate_ttl(ttl)?;
                // Round positive sub-millisecond durations up to 1ms so Redis
                // millisecond TTLs never truncate a positive window to 0ms.
                let ttl_ms: i64 = match ttl {
                    Some(t) => i64::try_from(t.as_millis().max(1)).unwrap_or(i64::MAX),
                    None => -1,
                };

                let mut conn = self
                    .pool
                    .get()
                    .await
                    .map_err(|e| CacheError::Backend(e.to_string()))?;

                let res: i32 = redis::Script::new(REDIS_RECORD_KEY_SCRIPT)
                    .key(&k)
                    .arg(ttl_ms)
                    .arg(max_keys)
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
            Box::pin(async move {
                let mut conn = self
                    .pool
                    .get()
                    .await
                    .map_err(|e| CacheError::Backend(e.to_string()))?;

                let count: usize = redis::Script::new(REDIS_COUNT_SCRIPT)
                    .key(&k)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_acquire_lease_and_release_lease_never_evicts_replacement_lease() {
        tokio::time::pause();

        let store = InMemoryTenantQuotaStore::new();
        let tenant = "tenant-lease-isolation";

        // 1. Request A acquires lease_a with a short 20ms TTL at max_keys=1
        let lease_a = store
            .acquire_lease(tenant, 1, Duration::from_millis(20))
            .await
            .unwrap()
            .expect("lease_a should be acquired");

        // 2. Lease A expires while request A is still running
        tokio::time::advance(Duration::from_millis(30)).await;

        // 3. Request B acquires replacement lease_b with 60s TTL
        let lease_b = store
            .acquire_lease(tenant, 1, Duration::from_secs(60))
            .await
            .unwrap()
            .expect("lease_b should be acquired after lease_a expired");

        // 4. Late completion of request A releases lease_a -> must be a no-op
        store.release_lease(tenant, &lease_a).await.unwrap();
        assert_eq!(
            store.count(tenant).await.unwrap(),
            1,
            "releasing expired lease_a must not evict active lease_b"
        );

        // 5. Third request is still blocked while lease_b is active
        assert!(
            store
                .acquire_lease(tenant, 1, Duration::from_secs(60))
                .await
                .unwrap()
                .is_none()
        );

        // 6. Releasing lease_b frees the slot
        store.release_lease(tenant, &lease_b).await.unwrap();
        assert_eq!(store.count(tenant).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn test_in_memory_quota_rejects_zero_window_and_allows_sub_ms() {
        let store = InMemoryTenantQuotaStore::new();
        let err = store
            .acquire("t1", 1, Duration::ZERO)
            .await
            .expect_err("Duration::ZERO must be rejected");
        assert!(err.to_string().contains("greater than zero"));

        let err = store
            .acquire_lease("t1", 1, Duration::ZERO)
            .await
            .expect_err("Duration::ZERO must be rejected");
        assert!(err.to_string().contains("greater than zero"));

        let err = store
            .record_key("t1", "k1", Some(Duration::ZERO), 1)
            .await
            .expect_err("Duration::ZERO must be rejected");
        assert!(err.to_string().contains("greater than zero"));

        assert!(
            store
                .acquire("t1", 1, Duration::from_micros(500))
                .await
                .unwrap()
        );
    }

    #[cfg(feature = "redis")]
    #[tokio::test]
    async fn test_redis_quota_rejects_zero_window_and_uses_redis_time() {
        let cfg = deadpool_redis::Config::from_url("redis://127.0.0.1:6379");
        let pool = cfg
            .create_pool(Some(deadpool_redis::Runtime::Tokio1))
            .expect("pool config");
        let store = RedisTenantQuotaStore::new(pool);

        let err = store
            .record_key("t1", "k1", Some(Duration::ZERO), 1)
            .await
            .expect_err("RedisTenantQuotaStore must reject Duration::ZERO");
        assert!(err.to_string().contains("greater than zero"));

        assert!(redis_store::REDIS_RECORD_KEY_SCRIPT.contains("redis.call('TIME')"));
        assert!(redis_store::REDIS_COUNT_SCRIPT.contains("redis.call('TIME')"));
    }

    #[cfg(feature = "redis")]
    #[test]
    fn redis_quota_key_avoids_double_prefix_and_script_sets_rolling_pexpire() {
        let cfg = deadpool_redis::Config::from_url("redis://127.0.0.1:6379");
        let pool = cfg
            .create_pool(Some(deadpool_redis::Runtime::Tokio1))
            .expect("pool config");
        let store = RedisTenantQuotaStore::new(pool.clone());
        assert_eq!(store.key("t1"), "rume:quota:{t1}:keys");

        let custom = RedisTenantQuotaStore::new(pool).with_prefix("custom:quota:");
        assert_eq!(custom.key("t1"), "custom:quota:{t1}:keys");

        let script = redis_store::REDIS_RECORD_KEY_SCRIPT;
        assert!(
            script.contains("ZREVRANGE") && script.contains("WITHSCORES"),
            "REDIS_RECORD_KEY_SCRIPT must inspect max score via ZREVRANGE ... WITHSCORES"
        );
        assert!(
            script.contains("PEXPIRE") && script.contains("PERSIST"),
            "REDIS_RECORD_KEY_SCRIPT must maintain rolling PEXPIRE / PERSIST on KEYS[1]"
        );
    }
}
