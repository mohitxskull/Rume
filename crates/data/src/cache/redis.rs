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

/// Chunk size for `SSCAN` + pipelined `UNLINK` during tag invalidation (`RC-22`).
const TAG_INVALIDATION_SCAN_COUNT: usize = 256;

/// O(1) Lua script that atomically detaches a tag set by renaming it to a unique staging key
/// and unlinking its persist marker. All three keys are declared in `KEYS` and share the
/// backend's `{...}` cluster hash tag so it never triggers `CROSSSLOT` or blocks Redis with
/// `SMEMBERS` + `DEL` (`RC-22`).
const DETACH_TAG_SCRIPT: &str = r#"
    if redis.call('EXISTS', KEYS[1]) == 0 then
        redis.call('UNLINK', KEYS[3])
        return 0
    end
    redis.call('RENAME', KEYS[1], KEYS[2])
    redis.call('UNLINK', KEYS[3])
    return 1
"#;

/// Ensures a Redis cache prefix carries a non-empty `{...}` cluster hash tag so multi-key
/// Lua scripts and chunked `UNLINK` batches always map to one Redis Cluster hash slot (`RC-22`).
fn ensure_cache_cluster_hash_tag(prefix: &str) -> String {
    if let Some(open) = prefix.find('{')
        && let Some(close_rel) = prefix[open + 1..].find('}')
        && close_rel > 0
    {
        return prefix.to_string();
    }
    let trimmed = prefix.trim_end_matches(':');
    if trimmed.is_empty() {
        "{rume:cache}:".to_string()
    } else if prefix.ends_with(':') {
        format!("{{{trimmed}}}:")
    } else {
        format!("{{{trimmed}}}")
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
            prefix: "{rume:cache}:".to_string(),
        })
    }

    /// Creates a Redis cache backend from an existing `deadpool_redis::Pool`.
    pub fn from_pool(pool: Pool) -> Self {
        Self {
            pool,
            prefix: "{rume:cache}:".to_string(),
        }
    }

    /// Configures a custom key prefix for cache namespace isolation.
    ///
    /// Automatically wraps untagged prefixes in a Redis Cluster `{...}` hash tag so
    /// tagged writes and tag invalidations never trigger `CROSSSLOT`.
    pub fn with_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.prefix = ensure_cache_cluster_hash_tag(&prefix.into());
        self
    }

    /// Returns a reference to the underlying connection pool.
    pub fn pool(&self) -> &Pool {
        &self.pool
    }

    fn prefixed_key(&self, key: &str) -> String {
        // Why: separate value keys (`{prefix}val:{key}`) and tag set keys
        // (`{prefix}tag:{tag}`) into disjoint subspaces so a user key like
        // `"tag:users"` can never collide with tag set `"users"` (`RC-06`).
        format!("{}val:{}", self.prefix, key)
    }

    fn tag_set_key(&self, tag: &str) -> String {
        format!("{}tag:{}", self.prefix, tag)
    }

    fn tag_evict_key(&self, tag: &str, token: &str) -> String {
        format!("{}tag_evict:{}:{}", self.prefix, tag, token)
    }

    fn tag_persist_key(&self, tag: &str) -> String {
        // Why: place persist markers in a disjoint `tag_persist:` subspace rather than
        // `tag:<t>:persist` so a user tag literally named `"foo:persist"` can never
        // collide with the persist marker for tag `"foo"` and trigger `WRONGTYPE` (`RC-23`).
        format!("{}tag_persist:{}", self.prefix, tag)
    }

    fn clear_pattern(&self) -> String {
        format!("{}*", escape_redis_glob(&self.prefix))
    }

    fn clear_prefix_patterns(&self, prefix: &str) -> [String; 3] {
        let escaped_base = escape_redis_glob(&self.prefix);
        let escaped_ns = escape_redis_glob(prefix);
        [
            format!("{escaped_base}val:{escaped_ns}*"),
            format!("{escaped_base}tag:{escaped_ns}*"),
            format!("{escaped_base}tag_persist:{escaped_ns}*"),
        ]
    }

    async fn scan_and_delete(
        conn: &mut deadpool_redis::Connection,
        pattern: &str,
    ) -> Result<(), CacheError> {
        let mut cursor: u64 = 0;
        loop {
            let (next_cursor, keys): (u64, Vec<String>) = redis::cmd("SCAN")
                .arg(cursor)
                .arg("MATCH")
                .arg(pattern)
                .arg("COUNT")
                .arg(100)
                .query_async(&mut **conn)
                .await
                .map_err(|e| CacheError::Backend(e.to_string()))?;

            if !keys.is_empty() {
                let mut unlink_cmd = redis::cmd("UNLINK");
                for k in &keys {
                    unlink_cmd.arg(k);
                }
                unlink_cmd
                    .query_async::<()>(&mut **conn)
                    .await
                    .map_err(|e| CacheError::Backend(e.to_string()))?;
            }

            cursor = next_cursor;
            if cursor == 0 {
                break;
            }
        }
        Ok(())
    }
}

/// Upper bound on millisecond TTLs sent to Redis (`i64::MAX / 2` ≈ 146 million years).
///
/// Why: clamping before casting to `i64` guarantees `val_ttl_arg` and `tag_ttl_arg`
/// stay strictly positive for any `Some(Duration)` and never wrap negative to trigger
/// the Lua `if tag_ttl < 0 then PERSIST` branch (`RC-23`).
const MAX_REDIS_TTL_MS: i64 = i64::MAX / 2;

fn clamp_redis_ttl_ms(ttl: Duration) -> i64 {
    ttl.as_millis().clamp(1, MAX_REDIS_TTL_MS as u128) as i64
}

fn redis_ttl_args(ttl: Option<Duration>) -> (i64, i64) {
    match ttl {
        Some(t) => {
            let val_ms = clamp_redis_ttl_ms(t);
            let tag_ms = val_ms.saturating_add(3_600_000).min(MAX_REDIS_TTL_MS);
            (val_ms, tag_ms)
        }
        None => (-1, -1),
    }
}

/// Escapes Redis `SCAN MATCH` glob metacharacters (`*`, `?`, `[`, `]`, `\`).
fn escape_redis_glob(input: &str) -> String {
    let mut escaped = String::with_capacity(input.len());
    for ch in input.chars() {
        if matches!(ch, '*' | '?' | '[' | ']' | '\\') {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    escaped
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
                // Why: Redis `SET ... PX` rejects `0` with `ERR invalid expire time`.
                // Clamp sub-millisecond and zero durations to 1ms and extreme values to
                // `MAX_REDIS_TTL_MS`, matching `set_with_tags`.
                let px_ms = clamp_redis_ttl_ms(ttl);
                cmd.arg("PX").arg(px_ms);
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
        let tag_key_pairs: Vec<(String, String)> = tags
            .iter()
            .map(|t| (self.tag_set_key(t), self.tag_persist_key(t)))
            .collect();
        Box::pin(async move {
            let mut conn = self
                .pool
                .get()
                .await
                .map_err(|e| CacheError::Backend(e.to_string()))?;

            if tag_key_pairs.is_empty() {
                let mut cmd = redis::cmd("SET");
                cmd.arg(&k).arg(&value);
                if let Some(ttl) = ttl {
                    cmd.arg("PX").arg(clamp_redis_ttl_ms(ttl));
                }
                cmd.query_async::<()>(&mut *conn)
                    .await
                    .map_err(|e| CacheError::Backend(e.to_string()))?;
                return Ok(());
            }

            // Why: publish the value and all of its tag memberships + tag TTL updates inside
            // one atomic Lua script so cancelling the Rust future mid-flight can never leave
            // an untagged value visible in Redis that `invalidate_tag` would miss.
            //
            // Why: validate every key's role and Redis type BEFORE the first write. Redis does
            // not undo a `SET` when a later `SADD` fails with WRONGTYPE, so a tag key that
            // already holds a string would leave the value written but untagged; the next
            // leader would then read it as a hit and report success without tag membership.
            // Missing tag sets and existing sets stay valid.
            const SET_WITH_TAGS_SCRIPT: &str = r#"
                for i = 2, #KEYS, 2 do
                    local tk = KEYS[i]
                    local pk = KEYS[i + 1]
                    if tk == KEYS[1] or pk == KEYS[1] then
                        return redis.error_reply('WRONGTYPE value key collides with a tag key')
                    end
                    local tt = redis.call('TYPE', tk)['ok']
                    if tt ~= 'none' and tt ~= 'set' then
                        return redis.error_reply('WRONGTYPE tag key is not a set')
                    end
                    local pt = redis.call('TYPE', pk)['ok']
                    if pt ~= 'none' and pt ~= 'string' then
                        return redis.error_reply('WRONGTYPE tag persist key is not a string')
                    end
                end
                local val_ttl = tonumber(ARGV[2])
                if val_ttl < 0 then
                    redis.call('SET', KEYS[1], ARGV[1])
                else
                    redis.call('SET', KEYS[1], ARGV[1], 'PX', val_ttl)
                end
                local tag_ttl = tonumber(ARGV[3])
                for i = 2, #KEYS, 2 do
                    local tk = KEYS[i]
                    local pk = KEYS[i + 1]
                    redis.call('SADD', tk, KEYS[1])
                    if tag_ttl < 0 then
                        redis.call('SET', pk, '1')
                        redis.call('PERSIST', tk)
                    elseif redis.call('EXISTS', pk) == 1 then
                        redis.call('PERSIST', tk)
                    else
                        local cur = redis.call('PTTL', tk)
                        if cur == -2 or cur == -1 or cur < tag_ttl then
                            redis.call('PEXPIRE', tk, tag_ttl)
                        end
                    end
                end
                return 1
            "#;

            let (val_ttl_arg, tag_ttl_arg) = redis_ttl_args(ttl);

            let script = redis::Script::new(SET_WITH_TAGS_SCRIPT);
            let mut invocation = script.prepare_invoke();
            invocation.key(&k);
            for (tk, persist_key) in &tag_key_pairs {
                invocation.key(tk).key(persist_key);
            }
            invocation
                .arg(&value)
                .arg(val_ttl_arg)
                .arg(tag_ttl_arg)
                .invoke_async::<()>(&mut *conn)
                .await
                .map_err(|e| CacheError::Backend(e.to_string()))?;

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

    fn invalidate_tag_keys(&self, tag: &str) -> BoxFuture<'_, Result<Vec<String>, CacheError>> {
        let tk = self.tag_set_key(tag);
        let staging_key = self.tag_evict_key(tag, &uuid::Uuid::new_v4().simple().to_string());
        let persist_key = self.tag_persist_key(tag);
        let val_prefix = format!("{}val:", self.prefix);
        Box::pin(async move {
            let mut conn = self
                .pool
                .get()
                .await
                .map_err(|e| CacheError::Backend(e.to_string()))?;

            // Why: atomically detach the tag set in O(1) via `RENAME` + `UNLINK` inside Lua
            // (all 3 keys declared in `KEYS` and co-located in one `{...}` cluster hash slot),
            // then drain members outside Lua via chunked `SSCAN` + pipelined `UNLINK` so tag
            // invalidation never blocks Redis with `O(N)` `SMEMBERS`/`DEL` in Lua and never
            // triggers `CROSSSLOT` on Redis Cluster (`RC-22`).
            let detached: i32 = redis::Script::new(DETACH_TAG_SCRIPT)
                .key(&tk)
                .key(&staging_key)
                .key(&persist_key)
                .invoke_async(&mut *conn)
                .await
                .map_err(|e| CacheError::Backend(e.to_string()))?;

            if detached == 0 {
                return Ok(Vec::new());
            }

            let mut deleted = Vec::new();
            let mut seen = std::collections::HashSet::new();
            let mut cursor: u64 = 0;

            loop {
                let (next_cursor, members): (u64, Vec<String>) = redis::cmd("SSCAN")
                    .arg(&staging_key)
                    .arg(cursor)
                    .arg("COUNT")
                    .arg(TAG_INVALIDATION_SCAN_COUNT)
                    .query_async(&mut *conn)
                    .await
                    .map_err(|e| CacheError::Backend(e.to_string()))?;

                let batch: Vec<String> = members
                    .into_iter()
                    .filter(|m| seen.insert(m.clone()))
                    .collect();

                if !batch.is_empty() {
                    let mut pipe = redis::pipe();
                    for member_key in &batch {
                        pipe.cmd("UNLINK").arg(member_key);
                    }
                    let counts: Vec<u64> = pipe
                        .query_async(&mut *conn)
                        .await
                        .map_err(|e| CacheError::Backend(e.to_string()))?;

                    for (member_key, count) in batch.into_iter().zip(counts) {
                        if count > 0 {
                            let logical = member_key
                                .strip_prefix(&val_prefix)
                                .unwrap_or(&member_key)
                                .to_string();
                            deleted.push(logical);
                        }
                    }
                }

                cursor = next_cursor;
                if cursor == 0 {
                    break;
                }
            }

            let _: () = redis::cmd("UNLINK")
                .arg(&staging_key)
                .query_async(&mut *conn)
                .await
                .map_err(|e| CacheError::Backend(e.to_string()))?;

            Ok(deleted)
        })
    }

    fn invalidate_tag(&self, tag: &str) -> BoxFuture<'_, Result<usize, CacheError>> {
        let tag = tag.to_string();
        Box::pin(async move {
            let keys = self.invalidate_tag_keys(&tag).await?;
            Ok(keys.len())
        })
    }

    fn clear(&self) -> BoxFuture<'_, Result<(), CacheError>> {
        let pattern = self.clear_pattern();
        Box::pin(async move {
            let mut conn = self
                .pool
                .get()
                .await
                .map_err(|e| CacheError::Backend(e.to_string()))?;

            Self::scan_and_delete(&mut conn, &pattern).await
        })
    }

    fn clear_prefix(&self, prefix: &str) -> BoxFuture<'_, Result<(), CacheError>> {
        let patterns = self.clear_prefix_patterns(prefix);
        Box::pin(async move {
            let mut conn = self
                .pool
                .get()
                .await
                .map_err(|e| CacheError::Backend(e.to_string()))?;

            for pattern in &patterns {
                Self::scan_and_delete(&mut conn, pattern).await?;
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn value_and_tag_keys_are_disjoint_and_cluster_hash_tagged() {
        fn extract_hash_tag(key: &str) -> Option<&str> {
            let start = key.find('{')? + 1;
            let end = start + key[start..].find('}')?;
            (end > start).then_some(&key[start..end])
        }

        let backend = RedisCacheBackend::new("redis://127.0.0.1:6379")
            .expect("pool")
            .with_prefix("rume:cache:");
        assert_ne!(
            backend.prefixed_key("tag:users"),
            backend.tag_set_key("users"),
            "value key `tag:users` must never collide with tag set key `users`"
        );
        assert_eq!(
            backend.prefixed_key("tag:users"),
            "{rume:cache}:val:tag:users"
        );
        assert_eq!(backend.tag_set_key("users"), "{rume:cache}:tag:users");
        assert_ne!(
            backend.tag_persist_key("foo"),
            backend.tag_set_key("foo:persist"),
            "tag persist key for `foo` must never collide with tag set key `foo:persist`"
        );
        assert_eq!(
            backend.tag_persist_key("foo"),
            "{rume:cache}:tag_persist:foo"
        );

        // All key subspaces share the exact same Redis Cluster `{...}` hash tag (`RC-22`).
        let keys = [
            backend.prefixed_key("k1"),
            backend.tag_set_key("users"),
            backend.tag_persist_key("users"),
            backend.tag_evict_key("users", "tok1"),
        ];
        let tag = extract_hash_tag(&keys[0]).expect("cluster hash tag required");
        for k in &keys[1..] {
            assert_eq!(extract_hash_tag(k), Some(tag));
        }

        // Detach Lua script must be O(1) without SMEMBERS or undeclared DELs.
        assert!(!DETACH_TAG_SCRIPT.contains("SMEMBERS"));
        assert!(!DETACH_TAG_SCRIPT.contains("ipairs"));
        assert!(DETACH_TAG_SCRIPT.contains("RENAME"));
        assert!(DETACH_TAG_SCRIPT.contains("UNLINK"));
    }

    #[test]
    fn glob_metacharacters_are_escaped_in_scan_patterns() {
        assert_eq!(escape_redis_glob(r"a*b?c[0]d\e"), r"a\*b\?c\[0\]d\\e");

        let backend = RedisCacheBackend::new("redis://127.0.0.1:6379")
            .expect("pool")
            .with_prefix("app[1]*:");
        assert_eq!(backend.clear_pattern(), r"{app\[1\]\*}:*");
        assert_eq!(
            backend.clear_prefix_patterns(r"t?a[0]\b:"),
            [
                r"{app\[1\]\*}:val:t\?a\[0\]\\b:*".to_string(),
                r"{app\[1\]\*}:tag:t\?a\[0\]\\b:*".to_string(),
                r"{app\[1\]\*}:tag_persist:t\?a\[0\]\\b:*".to_string(),
            ]
        );
    }

    #[test]
    fn redis_ttl_args_clamps_large_durations_without_negative_wrap() {
        let (val_ms, tag_ms) = redis_ttl_args(Some(Duration::MAX));
        assert_eq!(val_ms, MAX_REDIS_TTL_MS);
        assert_eq!(tag_ms, MAX_REDIS_TTL_MS);
        assert!(val_ms > 0);
        assert!(tag_ms > 0);

        // Duration just above i64::MAX ms previously wrapped negative on `as i64`
        let over_i64_max = Duration::from_millis((i64::MAX as u64) + 1);
        let (val2, tag2) = redis_ttl_args(Some(over_i64_max));
        assert!(val2 > 0 && val2 <= MAX_REDIS_TTL_MS);
        assert!(tag2 > 0 && tag2 <= MAX_REDIS_TTL_MS);

        assert_eq!(redis_ttl_args(None), (-1, -1));
    }
}
