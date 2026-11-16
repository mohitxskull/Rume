# TODO — Rume Architecture & Security Review (v06) Remediation Plan

Tracking remediation for findings in the 2026-11-08 Snapshot 2 audit report.

---

## Group 1 — Idempotency Lock Renewal & Worker Reliability (P0 / P1)

- [x] **Jobs R6 — Fail closed on default `IdempotencyStore::extend_lock_with_token`**
      `crates/core/src/jobs/reliability.rs`: Trait default returns `Ok(())` silently. Change default to return `Err(JobError::Unsupported("lock extension is not supported by this store".into()))`.
- [x] **Jobs R7 — Worker treats renewal failure as loss of idempotency ownership**
      `crates/core/src/jobs/worker.rs`: On `store.extend_lock_with_token` error in heartbeat loop, set cancellation flag and prevent job completion from acknowledging as safely idempotent.
- [x] **Jobs R8 — Comprehensive tests for renewal failure, token mismatch & unsupported store**
      Add tests covering: Redis outage during renewal, token mismatch during renewal, job running beyond 300s, and custom store returning `Unsupported`.

---

## Group 2 — Rate Limiting Trusted-Proxy Chain & Overflow Policy (P1)

- [x] **Resilience R3 — Right-to-left forwarded chain parsing with trusted proxy skipping**
      `crates/core/src/resilience/rate_limiter.rs`: Parse `X-Forwarded-For` right-to-left, validate IP addresses, skip hops in `trusted_proxies`, and pick the first untrusted IP rather than trusting the first element.
- [x] **Resilience R4 — Secure rate limiter overflow policy without evicting active identities**
      `crates/core/src/resilience/rate_limiter.rs`: When shard reaches `max_entries_per_shard`, route new unmapped keys to a shared `__overflow__` bucket rather than evicting active clients and resetting their rate limits.

---

## Group 3 — Persistence & MySQL Migration Transaction / Lock Ordering (P0 / P1)

- [x] **Persistence R3 — Commit transaction before releasing MySQL migration lock & propagate errors**
      `crates/core/src/persistence/migration.rs`: In `run_pending_backend` and `rollback_last_backend`, commit transaction (`tx.commit().await?`) before releasing named lock (`MYSQL_MIGRATION_UNLOCK`). Propagate both commit and unlock errors. Add tests.

---

## Group 4 — Tenant Quota TTL Expiration & Trait Contracts (P1 / P2)

- [x] **Tenant R2 — Make `record_key` and `remove_key` mandatory on `TenantQuotaStore`**
      `crates/core/src/tenant/quota.rs`: Remove default implementation that silently delegates `record_key` to `acquire()`, enforcing unique-key contracts on all implementations.
- [x] **Tenant R3 — Implement TTL tracking and lazy pruning in `InMemoryTenantQuotaStore`**
      `crates/core/src/tenant/quota.rs`: Track `(Instant of expiry)` for keys with TTL in `keys: Arc<RwLock<HashMap<String, HashMap<String, Option<Instant>>>>>`. Prune expired keys on `record_key`, `remove_key`, `count`, and `acquire`.
- [x] **Tenant R4 — Add tests for TTL expiry and concurrent same-key updates**
      Add unit and integration tests verifying TTL pruning and non-interference on same-key writes.

---

## Group 5 — Outbox Drain Timeout & Observability Response Preservation (P1 / P2)

- [x] **Outbox R2 — Enforce single absolute deadline on all drain operations including `pending_count`**
      `crates/core/src/outbox/relay.rs`: Establish `deadline = Instant::now() + timeout`. Wrap `self.repository.pending_count()` and sleep in `tokio::time::timeout(remaining, ...)`. Add test with stalled `pending_count()`.
- [x] **Observability R3 — Preserve original response bodies on inspection failure without `Body::empty()`**
      `crates/core/src/app/observability.rs`: Never replace response body with `Body::empty()` on inspection error. If inspection fails or exceeds limits, preserve and return the original body stream intact. Add test.

---

## Group 6 — CI Matrix & Multi-Instance Verification (P1)

- [x] **CI R1 — Feature matrix and multi-instance test coverage in CI workflow**
      `.github/workflows/ci.yml`: Ensure MySQL, PostgreSQL, SQLite, Redis feature matrix and multi-instance tests are explicitly configured in automated checks.
