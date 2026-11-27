//! Redis-backed leader election.

use std::fmt::Debug;
use std::time::Duration;

use deadpool_redis::Pool;

use crate::cache::redis::RedisCacheBackend;
use crate::coordination::error::CoordinationError;
use crate::coordination::leader::{LeaderElector, LeaderLease};
use crate::coordination::lock::{
    CoordinationFuture, checked_lease_deadline, conservative_lease_deadline,
};

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

    fn key_base(&self, role: &str) -> String {
        // Why: length-prefixing both `prefix` and `role` inside a shared `{...}`
        // hash tag prevents cross-prefix/role collisions (e.g. `app:` + `r` vs
        // `app:term:` + `r`), guarantees a non-empty hash tag even when `role == ""`
        // (preventing Redis Cluster `CROSSSLOT` errors), and keeps each leader/term
        // pair on the same hash slot (`RC-06`). Distinct `:leader` / `:term` suffixes
        // ensure leader keys and term counters can never alias.
        format!(
            "{{{}:{}:{}:{}}}",
            self.prefix.len(),
            self.prefix,
            role.len(),
            role
        )
    }

    fn leader_key(&self, role: &str) -> String {
        format!("{}:leader", self.key_base(role))
    }

    fn term_key(&self, role: &str) -> String {
        format!("{}:term", self.key_base(role))
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
                let deadline = checked_lease_deadline(pre_rtt, Duration::from_millis(effective_ms));
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

            match val {
                Some(s) => Ok(Some(parse_leader_value(&s, r)?)),
                None => Ok(None),
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

fn parse_leader_value(s: &str, role: String) -> Result<LeaderLease, CoordinationError> {
    let Some((len_str, rest)) = s.split_once(':') else {
        return Err(CoordinationError::Backend(format!(
            "malformed leader payload for role '{role}'"
        )));
    };
    let len = len_str.parse::<usize>().map_err(|_| {
        CoordinationError::Backend(format!("invalid leader payload length for role '{role}'"))
    })?;
    // Why: `len` is a byte length from Lua's `string.len`; check both the `:` separator
    // byte AND `rest.is_char_boundary(len)` before slicing `&rest[..len]` so corrupted
    // non-ASCII UTF-8 payloads (e.g. `"1:é:1"`) return an error instead of panicking (`RC-23`).
    if rest.as_bytes().get(len) != Some(&b':') || !rest.is_char_boundary(len) {
        return Err(CoordinationError::Backend(format!(
            "malformed leader payload boundary for role '{role}'"
        )));
    }
    let term = rest[len + 1..].parse::<u64>().map_err(|_| {
        CoordinationError::Backend(format!("invalid leader term for role '{role}'"))
    })?;
    Ok(LeaderLease {
        role,
        candidate_id: rest[..len].to_string(),
        term,
        expires_at: None,
    })
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
    fn leader_and_term_keys_are_disjoint_across_prefixes_and_non_empty_hash_tag() {
        let cfg = deadpool_redis::Config::from_url("redis://127.0.0.1:6379");
        let pool = cfg
            .create_pool(Some(deadpool_redis::Runtime::Tokio1))
            .expect("pool");
        let app = RedisLeaderElector::new(pool.clone()).with_prefix("app:");
        let app_term = RedisLeaderElector::new(pool).with_prefix("app:term:");

        assert_ne!(
            app.term_key("r"),
            app_term.leader_key("r"),
            "app: term_key must never collide with app:term: leader_key"
        );

        let keys = [
            app.leader_key("r"),
            app.term_key("r"),
            app_term.leader_key("r"),
            app_term.term_key("r"),
        ];
        for i in 0..keys.len() {
            for j in (i + 1)..keys.len() {
                assert_ne!(keys[i], keys[j], "keys must never collide: {keys:?}");
            }
        }
        assert_eq!(
            extract_hash_tag(&keys[0]),
            extract_hash_tag(&keys[1]),
            "leader and term keys must share a Redis Cluster hash tag"
        );

        let empty_leader = app.leader_key("");
        let empty_term = app.term_key("");
        let tag_l = extract_hash_tag(&empty_leader);
        let tag_t = extract_hash_tag(&empty_term);
        assert!(
            tag_l.is_some(),
            "empty role must still produce a non-empty hash tag: {empty_leader}"
        );
        assert_eq!(
            tag_l, tag_t,
            "empty role leader and term keys must share the same non-empty hash tag"
        );
    }

    #[test]
    fn parse_leader_value_rejects_non_char_boundary_utf8_without_panicking() {
        // `"é"` is 2 UTF-8 bytes; byte index 1 is mid-character and previously panicked.
        assert!(parse_leader_value("1:é:1", "scheduler".to_string()).is_err());

        // Valid multi-byte UTF-8 candidate with accurate byte length parses cleanly.
        let lease =
            parse_leader_value("2:é:7", "scheduler".to_string()).expect("valid utf8 leader");
        assert_eq!(lease.candidate_id, "é");
        assert_eq!(lease.term, 7);
    }
}
