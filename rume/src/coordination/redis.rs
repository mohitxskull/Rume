//! Production Redis-backed distributed locking and leader election.

use std::fmt::Debug;
use std::time::Duration;

use deadpool_redis::Pool;
use uuid::Uuid;

use crate::cache::redis::RedisCacheBackend;
use crate::coordination::error::CoordinationError;
use crate::coordination::leader::{LeaderElector, LeaderLease};
use crate::coordination::lock::{CoordinationFuture, DistributedLock, LockGuard};

/// Redis-backed implementation of [`DistributedLock`].
///
/// Guarantees:
/// - Mutual exclusion across separate OS processes and machines connected to Redis.
/// - Strictly monotonically increasing `fencing_token` issued via atomic Redis `INCR`.
/// - Automatic TTL lease expiration without requiring manual dead worker cleanup.
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

    fn lock_key(&self, resource: &str) -> String {
        format!("{}{}", self.prefix, resource)
    }

    fn fence_key(&self, resource: &str) -> String {
        format!("{}fence:{}", self.prefix, resource)
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
            let mut conn = self
                .pool
                .get()
                .await
                .map_err(|e| CoordinationError::Backend(e.to_string()))?;

            let lock_key = self.lock_key(&res);
            let fence_key = self.fence_key(&res);
            let lease_id = Uuid::new_v4().to_string();
            let ttl_ms = ttl.as_millis().max(1) as u64;

            // Lua script: atomically set lock key if not exists (NX) with TTL (PX)
            // and increment the monotonic fencing counter if acquired.
            let script = redis::Script::new(
                r#"
                local acquired = redis.call('SET', KEYS[1], ARGV[1], 'NX', 'PX', ARGV[2])
                if acquired then
                    local fence = redis.call('INCR', KEYS[2])
                    return fence
                else
                    return nil
                end
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
                }))
            } else {
                Ok(None)
            }
        })
    }

    fn renew<'a>(&'a self, guard: &'a LockGuard, ttl: Duration) -> CoordinationFuture<'a, ()> {
        Box::pin(async move {
            let mut conn = self
                .pool
                .get()
                .await
                .map_err(|e| CoordinationError::Backend(e.to_string()))?;

            let lock_key = self.lock_key(&guard.resource);
            let ttl_ms = ttl.as_millis().max(1) as u64;

            let script = redis::Script::new(
                r#"
                local current = redis.call('GET', KEYS[1])
                if current == ARGV[1] then
                    redis.call('PEXPIRE', KEYS[1], ARGV[2])
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

/// Redis-backed implementation of [`LeaderElector`].
///
/// Guarantees:
/// - Distributed leader election across independent processes/nodes.
/// - Monotonically increasing election terms backed by Redis `INCR`.
/// - Automatic leader lease expiration and failover.
#[derive(Clone)]
pub struct RedisLeaderElector {
    pool: Pool,
    prefix: String,
}

impl Debug for RedisLeaderElector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RedisLeaderElector")
            .field("prefix", &self.prefix)
            .finish()
    }
}

impl RedisLeaderElector {
    /// Creates a new leader election service from a `deadpool_redis::Pool`.
    pub fn new(pool: Pool) -> Self {
        Self {
            pool,
            prefix: "rume:leader:".to_string(),
        }
    }

    /// Creates a leader elector from a [`RedisCacheBackend`].
    pub fn from_backend(backend: &RedisCacheBackend) -> Self {
        Self {
            pool: backend.pool().clone(),
            prefix: "rume:leader:".to_string(),
        }
    }

    /// Configures a custom key prefix for leader election namespace isolation.
    pub fn with_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.prefix = prefix.into();
        self
    }

    fn leader_key(&self, role: &str) -> String {
        format!("{}{}", self.prefix, role)
    }

    fn term_key(&self, role: &str) -> String {
        format!("{}term:{}", self.prefix, role)
    }
}

impl LeaderElector for RedisLeaderElector {
    fn try_acquire<'a>(
        &'a self,
        role: &'a str,
        candidate_id: &'a str,
        ttl: Duration,
    ) -> CoordinationFuture<'a, Option<LeaderLease>> {
        let r = role.to_string();
        let c = candidate_id.to_string();
        Box::pin(async move {
            let mut conn = self
                .pool
                .get()
                .await
                .map_err(|e| CoordinationError::Backend(e.to_string()))?;

            let leader_key = self.leader_key(&r);
            let term_key = self.term_key(&r);
            let ttl_ms = ttl.as_millis().max(1) as u64;

            // Lua script: if leader key is not set, increment term and set candidate_id:term
            let script = redis::Script::new(
                r#"
                local current = redis.call('GET', KEYS[1])
                if not current then
                    local term = redis.call('INCR', KEYS[2])
                    local val = ARGV[1] .. ':' .. tostring(term)
                    redis.call('SET', KEYS[1], val, 'PX', ARGV[2])
                    return term
                else
                    return nil
                end
                "#,
            );

            let result: Option<i64> = script
                .key(&leader_key)
                .key(&term_key)
                .arg(&c)
                .arg(ttl_ms)
                .invoke_async(&mut *conn)
                .await
                .map_err(|e| CoordinationError::Backend(e.to_string()))?;

            if let Some(term) = result {
                Ok(Some(LeaderLease {
                    role: r,
                    candidate_id: c,
                    term: term as u64,
                }))
            } else {
                Ok(None)
            }
        })
    }

    fn renew<'a>(
        &'a self,
        role: &'a str,
        candidate_id: &'a str,
        ttl: Duration,
    ) -> CoordinationFuture<'a, Option<LeaderLease>> {
        let r = role.to_string();
        let c = candidate_id.to_string();
        Box::pin(async move {
            let mut conn = self
                .pool
                .get()
                .await
                .map_err(|e| CoordinationError::Backend(e.to_string()))?;

            let leader_key = self.leader_key(&r);
            let ttl_ms = ttl.as_millis().max(1) as u64;

            let script = redis::Script::new(
                r#"
                local current = redis.call('GET', KEYS[1])
                if current then
                    local prefix = ARGV[1] .. ':'
                    if string.sub(current, 1, string.len(prefix)) == prefix then
                        redis.call('PEXPIRE', KEYS[1], ARGV[2])
                        local term = string.sub(current, string.len(prefix) + 1)
                        return tonumber(term)
                    end
                end
                return nil
                "#,
            );

            let result: Option<i64> = script
                .key(&leader_key)
                .arg(&c)
                .arg(ttl_ms)
                .invoke_async(&mut *conn)
                .await
                .map_err(|e| CoordinationError::Backend(e.to_string()))?;

            if let Some(term) = result {
                Ok(Some(LeaderLease {
                    role: r,
                    candidate_id: c,
                    term: term as u64,
                }))
            } else {
                Ok(None)
            }
        })
    }

    fn get_leader<'a>(&'a self, role: &'a str) -> CoordinationFuture<'a, Option<LeaderLease>> {
        let r = role.to_string();
        Box::pin(async move {
            let mut conn = self
                .pool
                .get()
                .await
                .map_err(|e| CoordinationError::Backend(e.to_string()))?;

            let leader_key = self.leader_key(&r);
            let val: Option<String> = redis::cmd("GET")
                .arg(&leader_key)
                .query_async(&mut *conn)
                .await
                .map_err(|e| CoordinationError::Backend(e.to_string()))?;

            if let Some(s) = val
                && let Some((candidate, term_str)) = s.split_once(':')
                && let Ok(term) = term_str.parse::<u64>()
            {
                Ok(Some(LeaderLease {
                    role: r,
                    candidate_id: candidate.to_string(),
                    term,
                }))
            } else {
                Ok(None)
            }
        })
    }

    fn resign<'a>(&'a self, role: &'a str, candidate_id: &'a str) -> CoordinationFuture<'a, bool> {
        let r = role.to_string();
        let c = candidate_id.to_string();
        Box::pin(async move {
            let mut conn = self
                .pool
                .get()
                .await
                .map_err(|e| CoordinationError::Backend(e.to_string()))?;

            let leader_key = self.leader_key(&r);

            let script = redis::Script::new(
                r#"
                local current = redis.call('GET', KEYS[1])
                if current then
                    local prefix = ARGV[1] .. ':'
                    if string.sub(current, 1, string.len(prefix)) == prefix then
                        redis.call('DEL', KEYS[1])
                        return 1
                    end
                end
                return 0
                "#,
            );

            let result: i64 = script
                .key(&leader_key)
                .arg(&c)
                .invoke_async(&mut *conn)
                .await
                .map_err(|e| CoordinationError::Backend(e.to_string()))?;

            Ok(result == 1)
        })
    }
}
