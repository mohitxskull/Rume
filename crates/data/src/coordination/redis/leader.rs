//! Redis-backed leader election.

use std::fmt::Debug;
use std::time::Duration;

use deadpool_redis::Pool;

use crate::cache::redis::RedisCacheBackend;
use crate::coordination::error::CoordinationError;
use crate::coordination::leader::{LeaderElector, LeaderLease};
use crate::coordination::lock::{CoordinationFuture, conservative_lease_deadline};

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
        format!("{}{{{}}}", self.prefix, role)
    }

    fn term_key(&self, role: &str) -> String {
        format!("{}term:{{{}}}", self.prefix, role)
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
            // Why: Compute the conservative deadline before pool checkout and script dispatch
            // so pool wait and network RTT count against the local leader lease (`RC-01`).
            let (ttl_ms, deadline) = conservative_lease_deadline(ttl);

            let mut conn = self
                .pool
                .get()
                .await
                .map_err(|e| CoordinationError::Backend(e.to_string()))?;

            let leader_key = self.leader_key(&r);
            let term_key = self.term_key(&r);

            // Lua script: if leader key is not set, increment term and set length-prefixed candidate_id:term
            let script = redis::Script::new(
                r#"
                local current = redis.call('GET', KEYS[1])
                if not current then
                    local term = redis.call('INCR', KEYS[2])
                    local val = tostring(string.len(ARGV[1])) .. ':' .. ARGV[1] .. ':' .. tostring(term)
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
                    expires_at: Some(deadline),
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
            // Why: Capture `pre_rtt` before pool checkout and script dispatch (`RC-01`) and return
            // both the election term and the effective retained TTL (`max(PTTL, requested_ttl)`)
            // from Lua so `expires_at` reflects the actual retained Redis lease without post-RTT
            // inflation.
            let pre_rtt = std::time::Instant::now();
            let (ttl_ms, _) = conservative_lease_deadline(ttl);

            let mut conn = self
                .pool
                .get()
                .await
                .map_err(|e| CoordinationError::Backend(e.to_string()))?;

            let leader_key = self.leader_key(&r);

            let script = redis::Script::new(
                r#"
                if redis.replicate_commands then redis.replicate_commands() end
                local current = redis.call('GET', KEYS[1])
                if current then
                    local prefix = tostring(string.len(ARGV[1])) .. ':' .. ARGV[1] .. ':'
                    if string.sub(current, 1, string.len(prefix)) == prefix then
                        local requested_ttl = tonumber(ARGV[2])
                        local remaining_ttl = redis.call('PTTL', KEYS[1])
                        local effective_ttl = requested_ttl
                        if remaining_ttl < 0 or remaining_ttl < requested_ttl then
                            redis.call('PEXPIRE', KEYS[1], requested_ttl)
                        else
                            effective_ttl = remaining_ttl
                        end
                        local term = string.sub(current, string.len(prefix) + 1)
                        return {tonumber(term), effective_ttl}
                    end
                end
                return nil
                "#,
            );

            let result: Option<(i64, i64)> = script
                .key(&leader_key)
                .arg(&c)
                .arg(ttl_ms)
                .invoke_async(&mut *conn)
                .await
                .map_err(|e| CoordinationError::Backend(e.to_string()))?;

            if let Some((term, effective_ttl_ms)) = result {
                let effective_ms = u64::try_from(effective_ttl_ms.max(1)).unwrap_or(u64::MAX);
                Ok(Some(LeaderLease {
                    role: r,
                    candidate_id: c,
                    term: term as u64,
                    expires_at: Some(pre_rtt + Duration::from_millis(effective_ms)),
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
                && let Some((len_str, rest)) = s.split_once(':')
                && let Ok(len) = len_str.parse::<usize>()
                && rest.len() > len
                && &rest[len..=len] == ":"
                && let Ok(term) = rest[len + 1..].parse::<u64>()
            {
                let candidate = &rest[..len];
                Ok(Some(LeaderLease {
                    role: r,
                    candidate_id: candidate.to_string(),
                    term,
                    expires_at: None,
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
                    local prefix = tostring(string.len(ARGV[1])) .. ':' .. ARGV[1] .. ':'
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
