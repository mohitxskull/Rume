# Task 06 — Tenant Quota Default TTL Alignment & Redis Quota Key Hygiene (`RC-07`)

## Goal

Resolve `Cache`'s configured default TTL before recording tenant quota slots, roll back quota slots when cache writes fail due to backend or closed errors, eliminate the double `quota:` prefix in `RedisTenantQuotaStore::key()`, and maintain a rolling `PEXPIRE` / `PERSIST` TTL on the Redis quota sorted set key.

## Files touched

- `crates/data/src/cache/typed.rs` (`Cache::default_ttl`, `pub fn resolve_ttl`)
- `crates/security/src/tenant/cache.rs` (`TenantScopedCache::set_with_tags`)
- `crates/security/src/tenant/quota.rs` (`RedisTenantQuotaStore::key`, `REDIS_RECORD_KEY_SCRIPT`)

## Deliverable

1. **`crates/data/src/cache/typed.rs`:** Expose `pub fn default_ttl(&self) -> Duration` and make `pub fn resolve_ttl(&self, ttl: Option<Duration>) -> Option<Duration>` public so callers such as `TenantScopedCache` observe the exact effective TTL applied by `set_json_with_tags`.
2. **`crates/security/src/tenant/cache.rs` (`set_with_tags`):**
   - Resolve `let effective_ttl = self.cache.resolve_ttl(ttl);` once up front, pass `effective_ttl` to `quota.record_key(tenant_id, key, effective_ttl, self.max_keys_per_tenant)` AND to `self.cache.set_json_with_tags(&full_key, value, &tag_refs, effective_ttl)`.
   - On `res.is_err() && !key_already_existed`, remove the newly acquired quota slot unless `self.cache.exists(&full_key).await` returns `Ok(true)` (`!matches!(self.cache.exists(&full_key).await, Ok(true))`) — ensuring backend unreachable, serialization, and `CacheError::Closed` failures also release the quota slot.
3. **`crates/security/src/tenant/quota.rs`:**
   - Fix `RedisTenantQuotaStore::key(&self, tenant_id: &str)` (`:332-334`) from `format!("{}quota:{{{}}}:keys", self.prefix, tenant_id)` to `format!("{}{{{}}}:keys", self.prefix, tenant_id)` (removing the redundant `quota:` segment while preserving the `{tenant_id}` Redis Cluster hash tag).
   - Update `REDIS_RECORD_KEY_SCRIPT` (`:279-299`) after `ZADD` so the sorted set key `KEYS[1]` never leaks indefinitely when finite TTLs are used: inspect the highest-scoring member via `redis.call('ZREVRANGE', KEYS[1], 0, 0, 'WITHSCORES')`; if `ttl_ms < 0` or the max score is `+inf` (`inf`), call `redis.call('PERSIST', KEYS[1])`, otherwise call `redis.call('PEXPIRE', KEYS[1], math.max(1, math.ceil(max_score - now_ms)))` so shorter new keys never truncate longer existing entries.

## Constraints honoured

- `rules/70-code-rationale.md`:
  - `Why:` comment on `TenantScopedCache::set_with_tags` rollback explaining why `!matches!(self.cache.exists(&full_key).await, Ok(true))` is used as best-effort cleanup (a secondary `exists` error on `Backend`/`Closed` failure cannot prove the cache write persisted, whereas skipping rollback leaks the tenant's quota slot permanently).
  - `Why:` comment on `REDIS_RECORD_KEY_SCRIPT` `ZREVRANGE` max-score `PEXPIRE`/`PERSIST` logic explaining why blindly calling `PEXPIRE KEYS[1] ttl_ms` would prematurely wipe out longer-lived keys in the same tenant sorted set.
- `crates/security/src/tenant/quota.rs:534-579` (`validate_ttl` rejecting `Duration::ZERO`, Redis `TIME` usage) and `crates/core/tests/integration/tenant.rs` (`tenant_scoped_cache_tag_isolation_and_quota_bounds`).
- `crates/core/tests/containers/redis.rs:230-295` (`test_redis_cluster_wide_tenant_quota_coordination`).

## Named tests (bodies omitted)

- `tenant_cache_none_ttl_propagates_cache_default_ttl_to_quota_store` (`crates/security/src/tenant/cache.rs` `#[cfg(test)]`) — configures `CacheConfig::default().with_default_ttl(42)`, wraps a spy `TenantQuotaStore`, calls `set(..., None)`, and asserts `record_key` received `Some(Duration::from_secs(42))`.
- `tenant_cache_rolls_back_quota_on_closed_and_backend_errors` (`crates/security/src/tenant/cache.rs` `#[cfg(test)]`) — tests both `cache.close()` (`CacheError::Closed`) and a failing mock `CacheBackend` (`exists` returning `Err`), asserting `set` returns `Err` and the quota slot is removed (`active_key_count == 0`).
- `redis_quota_key_avoids_double_prefix_and_script_sets_rolling_pexpire` (`crates/security/src/tenant/quota.rs` `#[cfg(test)]`) — asserts `RedisTenantQuotaStore::new(pool).key("t1") == "rume:quota:{t1}:keys"` and `REDIS_RECORD_KEY_SCRIPT` maintains `PEXPIRE`/`PERSIST` via `ZREVRANGE`; plus container verification in `crates/core/tests/containers/redis.rs` confirming `PTTL > 0` for finite TTLs and longer existing member TTLs are not shortened by shorter subsequent keys.

## Verification

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -j 2 -- -D warnings
cargo test -p rume-data --all-features -j 2
cargo test -p rume-security --all-features -j 2
cargo test -p rume --test integration --all-features tenant -j 2
```

(And with Docker running: `cargo test -p rume --test containers --all-features test_redis_cluster_wide_tenant_quota_coordination -j 2`.)

Expected: 0 warnings and all data/security/tenant tests pass.

## Commit

Only when the user asks:

```text
fix(tenant): align quota TTL with cache default, rollback on backend errors, and expire Redis quota keys
```

## Done-when

- [ ] `Cache::default_ttl` and `Cache::resolve_ttl` are `pub` in `crates/data/src/cache/typed.rs`.
- [ ] `TenantScopedCache::set_with_tags` passes `resolve_ttl(ttl)` to `record_key` and rolls back on backend/closed errors with a `Why:` comment.
- [ ] `RedisTenantQuotaStore::key` produces `"{prefix}{{{tenant_id}}}:keys"` (no `quota:quota:`) and `REDIS_RECORD_KEY_SCRIPT` maintains `PEXPIRE`/`PERSIST`.
- [ ] All verification commands exit 0 with 0 warnings.
