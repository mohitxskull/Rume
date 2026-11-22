//! Redis-backed distributed locking.

use std::fmt::Debug;
use std::time::Duration;

use deadpool_redis::Pool;
use uuid::Uuid;

use crate::cache::redis::RedisCacheBackend;
use crate::coordination::error::CoordinationError;
use crate::coordination::lock::{
    CoordinationFuture, DistributedLock, LockGuard, conservative_lease_deadline,
};

/// Redis-backed implementation of [`DistributedLock`].
///
/// Guarantees (subject to Redis storage prerequisites below):
/// - Mutual exclusion across separate OS processes and machines connected to Redis.
/// - Strictly monotonically increasing `fencing_token` issued via atomic Redis `INCR`.
/// - Automatic TTL lease expiration without requiring manual dead worker cleanup.
///
/// # Redis Storage Prerequisites
///
/// The Redis instance backing this lock **must** be configured with `maxmemory-policy noeviction`
/// (or equivalent retention guarantee) so active lease keys and fencing counters are never
/// evicted under memory pressure. Under an evicting policy such as `allkeys-lru`, Redis can
/// evict an active lease before its local deadline (allowing a second caller to acquire the lock
/// concurrently) or evict a fencing counter (allowing future acquisitions to reuse earlier tokens).
/// Using a separate key prefix does **not** protect keys from eviction.
#[derive(Clone)]
pub struct RedisDistributedLock {
    pool: Pool,
    prefix: String,
}

impl Debug for RedisDistributedLock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RedisDistributedLock")
            .field("prefix", &self.prefix)
            .finish()
    }
}

impl RedisDistributedLock {
    /// Creates a new distributed lock service from a `deadpool_redis::Pool`.
    pub fn new(pool: Pool) -> Self {
        Self {
            pool,
            prefix: "rume:lock:".to_string(),
        }
    }

    /// Creates a lock service from a [`RedisCacheBackend`].
    ///
    /// # Prerequisites
    ///
    /// Only share a cache pool when the underlying Redis instance uses `noeviction` and retains
    /// active lease keys and fencing counters. A separate key prefix does not protect coordination
    /// keys from cache eviction policies such as `allkeys-lru`.
    pub fn from_backend(backend: &RedisCacheBackend) -> Self {
        Self {
            pool: backend.pool().clone(),
            prefix: "rume:lock:".to_string(),
        }
    }

    /// Configures a custom key prefix for lock namespace isolation.
    pub fn with_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.prefix = prefix.into();
        self
    }

    fn key_base(&self, resource: &str) -> String {
        // Why: length-prefixing both `prefix` and `resource` inside a shared `{...}`
        // hash tag prevents cross-prefix/resource collisions (e.g. `tenant:` + `report`
        // vs `tenant:fence:` + `report`) while keeping each lease/counter pair on the
        // same Redis Cluster hash slot. Distinct `:lease` / `:fence` suffixes ensure
        // lease keys and fencing counters can never alias.
        format!(
            "{{{}:{}:{}:{}}}",
            self.prefix.len(),
            self.prefix,
            resource.len(),
            resource
        )
    }

    fn lock_key(&self, resource: &str) -> String {
        format!("{}:lease", self.key_base(resource))
    }

    fn fence_key(&self, resource: &str) -> String {
        format!("{}:fence", self.key_base(resource))
    }
}

impl DistributedLock for RedisDistributedLock {
    fn acquire<'a>(
        &'a self,
        resource: &'a str,
        ttl: Duration,
    ) -> CoordinationFuture<'a, Option<LockGuard>> {
        let res = resource.to_string();
        Box::pin(async move {
            // Why: Compute the conservative deadline before pool checkout and script dispatch
            // so pool wait and network RTT count against the local lease deadline (`RC-01`).
            let (ttl_ms, deadline) = conservative_lease_deadline(ttl);

            let mut conn = self
                .pool
                .get()
                .await
                .map_err(|e| CoordinationError::Backend(e.to_string()))?;

            let lock_key = self.lock_key(&res);
            let fence_key = self.fence_key(&res);
            let lease_id = Uuid::new_v4().to_string();

            // Why: Check contention first, then increment the fencing counter before writing
            // the lease. Because Lua scripts run atomically in Redis, a contended call still
            // returns `nil` without incrementing the counter, and a failed `INCR` (e.g. if
            // the counter key holds a non-integer value) never leaves an orphan lease behind.
            let script = redis::Script::new(
                r#"
                if redis.call('EXISTS', KEYS[1]) == 1 then
                    return nil
                end
                local fence = redis.call('INCR', KEYS[2])
                redis.call('SET', KEYS[1], ARGV[1], 'PX', ARGV[2])
                return fence
                "#,
            );

            let result: Option<i64> = script
                .key(&lock_key)
                .key(&fence_key)
                .arg(&lease_id)
                .arg(ttl_ms)
                .invoke_async(&mut *conn)
                .await
                .map_err(|e| CoordinationError::Backend(e.to_string()))?;

            if let Some(token) = result {
                Ok(Some(LockGuard {
                    resource: res,
                    lease_id,
                    fencing_token: token as u64,
                    expires_at: Some(deadline),
                }))
            } else {
                Ok(None)
            }
        })
    }

    fn renew<'a>(&'a self, guard: &'a mut LockGuard, ttl: Duration) -> CoordinationFuture<'a, ()> {
        Box::pin(async move {
            // Why: Compute the conservative deadline before pool acquisition / script dispatch
            // so wait/network/scheduling delay counts against the local lease, and only advance
            // `guard.expires_at` monotonically once Redis confirms ownership and either extends
            // the lease TTL or confirms the remaining TTL already meets `ttl_ms`.
            let (ttl_ms, candidate_deadline) = conservative_lease_deadline(ttl);

            let mut conn = self
                .pool
                .get()
                .await
                .map_err(|e| CoordinationError::Backend(e.to_string()))?;

            let lock_key = self.lock_key(&guard.resource);

            let script = redis::Script::new(
                r#"
                if redis.replicate_commands then redis.replicate_commands() end
                local current = redis.call('GET', KEYS[1])
                if current == ARGV[1] then
                    local requested_ttl = tonumber(ARGV[2])
                    local remaining_ttl = redis.call('PTTL', KEYS[1])
                    if remaining_ttl < 0 or remaining_ttl < requested_ttl then
                        redis.call('PEXPIRE', KEYS[1], requested_ttl)
                    end
                    return 1
                else
                    return 0
                end
                "#,
            );

            let renewed: i64 = script
                .key(&lock_key)
                .arg(&guard.lease_id)
                .arg(ttl_ms)
                .invoke_async(&mut *conn)
                .await
                .map_err(|e| CoordinationError::Backend(e.to_string()))?;

            if renewed == 1 {
                guard.advance_expires_at(candidate_deadline);
                Ok(())
            } else {
                Err(CoordinationError::LockExpired {
                    resource: guard.resource.clone(),
                })
            }
        })
    }

    fn release<'a>(&'a self, guard: LockGuard) -> CoordinationFuture<'a, bool> {
        Box::pin(async move {
            let mut conn = self
                .pool
                .get()
                .await
                .map_err(|e| CoordinationError::Backend(e.to_string()))?;

            let lock_key = self.lock_key(&guard.resource);

            let script = redis::Script::new(
                r#"
                local current = redis.call('GET', KEYS[1])
                if current == ARGV[1] then
                    redis.call('DEL', KEYS[1])
                    return 1
                else
                    return 0
                end
                "#,
            );

            let released: i64 = script
                .key(&lock_key)
                .arg(&guard.lease_id)
                .invoke_async(&mut *conn)
                .await
                .map_err(|e| CoordinationError::Backend(e.to_string()))?;

            Ok(released == 1)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extract_hash_tag(key: &str) -> Option<&str> {
        let start = key.find('{')?;
        let rest = &key[start + 1..];
        let end = rest.find('}')?;
        (end > 0).then_some(&rest[..end])
    }

    #[test]
    fn lock_and_fence_keys_are_disjoint_across_prefixes_and_share_hash_tag() {
        let cfg = deadpool_redis::Config::from_url("redis://127.0.0.1:6379");
        let pool = cfg
            .create_pool(Some(deadpool_redis::Runtime::Tokio1))
            .expect("pool");
        let tenant = RedisDistributedLock::new(pool.clone()).with_prefix("tenant:");
        let tenant_fence = RedisDistributedLock::new(pool).with_prefix("tenant:fence:");

        let keys = [
            tenant.lock_key("report"),
            tenant.fence_key("report"),
            tenant_fence.lock_key("report"),
            tenant_fence.fence_key("report"),
        ];
        for i in 0..keys.len() {
            for j in (i + 1)..keys.len() {
                assert_ne!(keys[i], keys[j], "keys must never collide: {keys:?}");
            }
        }
        assert_eq!(
            extract_hash_tag(&keys[0]),
            extract_hash_tag(&keys[1]),
            "lease and fence keys for the same resource must share a Redis Cluster hash tag"
        );
        assert_eq!(
            extract_hash_tag(&keys[2]),
            extract_hash_tag(&keys[3]),
            "lease and fence keys for the same resource must share a Redis Cluster hash tag"
        );
    }
}
