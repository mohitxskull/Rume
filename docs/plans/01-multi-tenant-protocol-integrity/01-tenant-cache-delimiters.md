# Task 01 — Tenant Cache Delimiters & Scoping

## Goal

Prevent cross-tenant cache key collisions and unauthorized cache invalidation in [`rume/src/tenant/cache.rs`](file:///home/skull/Projects/rume/rume/src/tenant/cache.rs) by length-prefixing the tenant segment in scoped cache keys and tenant invalidation tags.

## Files touched

- [`rume/src/tenant/cache.rs`](file:///home/skull/Projects/rume/rume/src/tenant/cache.rs)

## Deliverable

1. Update `TenantScopedCache::scoped_key(tenant_id: &str, key: &str) -> String` to encode the tenant segment length explicitly: `format!("tenant:{}:{}:{key}", tenant_id.len(), tenant_id)`.
2. Update `TenantScopedCache::tenant_tag(tenant_id: &str) -> String` to encode the tenant segment length explicitly: `format!("tenant:{}:{tenant_id}", tenant_id.len())`.
3. Add a unit test module `#[cfg(test)] mod tests` in [`rume/src/tenant/cache.rs`](file:///home/skull/Projects/rume/rume/src/tenant/cache.rs) verifying delimiter collision resistance.

## Constraints honoured

- Preserve existing public API signatures on [`TenantScopedCache<B>`](file:///home/skull/Projects/rume/rume/src/tenant/cache.rs#L17-L66): `new`, `get`, `set`, `delete`, and `invalidate_tenant`.
- Preserve existing cache integration test semantics in [`rume/tests/tenant_suite.rs:139-191`](file:///home/skull/Projects/rume/rume/tests/tenant_suite.rs#L139-L191) (`test_tenant_scoped_cache`).
- Comply with [`.opencode/rules/70-code-rationale.md`](file:///home/skull/Projects/rume/.opencode/rules/70-code-rationale.md#L23-L36) by including a `Why:` rationale comment explaining why length-prefixing is used over regex character restriction.
- Comply with [`.opencode/rules/85-minimalism.md`](file:///home/skull/Projects/rume/.opencode/rules/85-minimalism.md#L8-L25): use `std` formatting without introducing external hashing or delimiter parsing dependencies.
- Closes Security C2 finding from [`docs/reports/reviews/v02/todo.md:22-23`](file:///home/skull/Projects/rume/docs/reports/reviews/v02/todo.md#L22-L23) and [`docs/reports/reviews/v02/scratch/group-A-findings.md:25-46`](file:///home/skull/Projects/rume/docs/reports/reviews/v02/scratch/group-A-findings.md#L25-L46).

## Named tests (bodies omitted)

- `scoped_key_disambiguates_colon_delimited_tenant_id_and_key`: asserts that `scoped_key("a:b", "c") != scoped_key("a", "b:c")`.
- `tenant_tag_disambiguates_colon_delimited_tenant_ids`: asserts that `tenant_tag("a:b") != tenant_tag("a")`.
- `tenant_scoped_cache_preserves_isolated_read_and_write_with_colons`: sets a value under tenant `"t1:user"` with key `"42"` and under tenant `"t1"` with key `"user:42"`, asserting that retrieving `"user:42"` under tenant `"t1"` returns `t1`'s value and does not leak or collide with `t1:user`.
- `tenant_scoped_cache_invalidation_does_not_leak_across_colon_boundaries`: invalidates tenant `"t1"`, asserting that cached keys belonging to tenant `"t1:user"` remain intact and accessible.

## Code-only decisions

- Length-prefixed delimiter format:
  ```rust
  // Why: Length-prefixing the tenant segment ("tenant:{len}:{tenant_id}:{key}") guarantees
  // unambiguous key segment boundaries without restricting allowed characters in tenant IDs
  // or keys (e.g. UUIDs, slashes, hyphens, or legacy colon identifiers).
  fn scoped_key(tenant_id: &str, key: &str) -> String {
      format!("tenant:{}:{}:{key}", tenant_id.len(), tenant_id)
  }

  // Why: Symmetrical length-prefixing on tenant invalidation tags ("tenant:{len}:{tenant_id}")
  // prevents tag collision where invalidating tenant "a" could inadvertently evict tenant "a:b".
  fn tenant_tag(tenant_id: &str) -> String {
      format!("tenant:{}:{tenant_id}", tenant_id.len())
  }
  ```

## Verification

```bash
cargo test -p rume tenant::cache
cargo clippy -p rume --all-targets
```

Expected output:
- `test tenant::cache::tests::scoped_key_disambiguates_colon_delimited_tenant_id_and_key ... ok`
- `test tenant::cache::tests::tenant_tag_disambiguates_colon_delimited_tenant_ids ... ok`
- `test tenant::cache::tests::tenant_scoped_cache_preserves_isolated_read_and_write_with_colons ... ok`
- `test tenant::cache::tests::tenant_scoped_cache_invalidation_does_not_leak_across_colon_boundaries ... ok`
- Clippy exits 0 with zero warnings.

## Commit

*Only when the user asks:*

```commit
fix(tenant): length-prefix tenant cache keys and invalidation tags

Prevent cross-tenant cache key collision and unauthorized tag
invalidation caused by unescaped colon delimiters. Closes Security C2.
```

## Done-when

- [ ] `TenantScopedCache::scoped_key("a:b", "c")` produces `"tenant:3:a:b:c"`, distinct from `"tenant:1:a:b:c"`.
- [ ] `TenantScopedCache::tenant_tag("a:b")` produces `"tenant:3:a:b"`, distinct from `"tenant:1:a"`.
- [ ] Unit tests in `rume/src/tenant/cache.rs` assert that cache reads, writes, and invalidations between tenants with overlapping prefixes and colon boundaries are strictly isolated.
- [ ] Existing cache tests in `rume/tests/tenant_suite.rs` pass with zero regressions.
