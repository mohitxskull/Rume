# 04-rc06-tenant-quota-sliding-window-ttl.md

## Goal

Fix RC-06: `TenantQuotaLayer` never releases quota after a successful request (acquire with no matching release → quota leak; a tenant is permanently blocked after `limit` requests), and quota keys carry no TTL so they never expire. Fix as a sliding-window rate limit with TTL matching `retry-after: 60`, with a configurable `window: Duration` on `TenantQuotaLayer` (default 60s).

Fix root cause:
- Add a `window: Duration` field to `TenantQuotaLayer` (default `Duration::from_secs(60)`), exposed via a `with_window` builder.
- Every `record_key` call passes `Some(window)` (or configured window) instead of `ttl = None` — in `TenantQuotaLayer::acquire` (via `store.acquire` → `record_key`) AND in `RedisTenantQuotaStore::acquire` and `InMemoryTenantQuotaStore::acquire`.
- Release the quota after the response is produced in `TenantQuotaService::call` (call `store.release(&tenant.id)` after `inner.call`), so the slot is freed for the next window.
- Replace `SystemTime::now()...as_nanos()` with `uuid::Uuid::new_v4()` in `RedisTenantQuotaStore::acquire` to avoid nanos-collision lease IDs.

NOT deletion, NOT concurrent-in-flight RAII.

## Files touched

- `crates/core/src/tenant/layer.rs` — `TenantQuotaLayer` (add `window` field, `with_window`, pass `Some(window)`), `TenantQuotaService::call` (release after response).
- `crates/core/src/tenant/quota.rs` — `InMemoryTenantQuotaStore::acquire` (pass `Some(ttl)`) and `RedisTenantQuotaStore::acquire` (uuid instead of nanos; pass `Some(ttl)`).

## Constraints honoured

- Sliding-window rate limit with TTL matching `retry-after: 60`; configurable `window: Duration`.
- Every `record_key` call passes `Some(Duration::from_secs(60))` (or configured window) instead of `ttl = None`.
- `uuid::Uuid::new_v4()` replaces `SystemTime::now()...as_nanos()` in Redis acquire.
- No new dependencies (uuid is already a dependency), no `unsafe`.
- Record a `Why:` comment noting release-after-response turns the fixed-window counter into a sliding window bounded by TTL expiry.

## Done when (checkable by a command)

- `cargo test -p rume-core tenant` passes (includes existing `test_tenant_quota_layer_enforces_tier_limits`).
- `cargo clippy -p rume-core --all-targets` and `cargo fmt --all --check` clean.
- `TenantQuotaLayer::new(store).with_window(Duration::from_secs(60))` compiles; `window` defaults to 60s.
- Grep confirms no `record_key(..., None, ...)` call sites remain in tenant quota code (all pass `Some(...)`); no `SystemTime::now()...as_nanos()` in `RedisTenantQuotaStore::acquire`.
- `store.release(&tenant.id)` is called on the success path of `TenantQuotaService::call`.

## Verification (fails-before test idea)

Extend `test_tenant_quota_layer_enforces_tier_limits` (or add a new test):

1. **Release:** a free tenant with limit 2 makes 2 requests (OK), then the 3rd is 429. After the window TTL elapses (or after release is exercised), a new request succeeds again → proves release + TTL expiry recycle the slot. Before the fix the 3rd and all subsequent requests stay 429 forever (leak).
2. **TTL:** `record_key` is invoked with `Some(60s)` — verify via an instrumented fake `TenantQuotaStore` that `record_key` receives a non-None TTL of 60s. Before the fix TTL is `None` → test fails.
3. **UUID lease:** `RedisTenantQuotaStore::acquire` builds its unique id from `Uuid::new_v4()` not nanos — verified by the existing acquire path compiling and a unit test asserting unique ids differ across calls within the same nanosecond.
