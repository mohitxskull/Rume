# Rume v03/diff/v01 Review — todo.md

Derived from the 6 lens reports in `docs/reports/reviews/v03/diff/v01/`:
- `outbox-data-integrity.md` (M1, m1)
- `pool-lifecycle-persistence.md` (C1, m1)
- `jobs-worker-concurrency.md` (M1–M2, m1–m2)
- `coordination-multi-tenant.md` (M1–M2, m1–m3)
- `security-protocols.md` (M1–M2, m1–m2)
- `testing-feature-matrix.md` (C1, M1–M4, m1–m2)

Ordered by priority tier (`P0` critical unblockers → `P2` minor polish), with findings sharing a root cause or subsystem grouped for solving together.

---

## P0 — Critical (Fix first: live database migration failures and broken single-feature test compilation)

> Fix immediately before any staging deployment or running test suites against single-feature backends.

### Group A — Migration Schema Divergence & Single-Feature Test Matrix (solve together)
> Root cause: Truncated DDL statements in secondary migration verification functions and feature-gated container imports in shared test utilities.

- [x] **Pool C1 — Schema divergence in `pending_migrations` and `verify_no_pending_backend` creates truncated 2-column `rume_migrations` table, breaking subsequent migrations** (`pool-lifecycle-persistence.md` C1, `testing-feature-matrix.md` M2).
      `rume/src/persistence/migration.rs:100, 253`: DDL in `pending_migrations` and `verify_no_pending_backend` creates `rume_migrations (id VARCHAR(255) PRIMARY KEY, applied_at BIGINT NOT NULL)`. When `run_pending_backend` is subsequently executed, `CREATE TABLE IF NOT EXISTS` is a no-op and `INSERT INTO rume_migrations (id, name, applied_at, checksum)` fails with `column "name" ... does not exist`, breaking `test_postgres_migrations_up_and_rollback`. Unify table creation across all functions to include `name` and `checksum` with defaults.
- [x] **Testing C1 — Single-feature cargo check and test builds fail for `mysql` and `redis` due to missing testcontainer imports in `common/containers.rs`** (`testing-feature-matrix.md` C1).
      `rume/tests/common/containers.rs:11–20, 81–93, 138–151`: Imports for `AsyncRunner` and `ContainerAsync` are gated under `#[cfg(feature = "postgres")]` only. Building tests under `--no-default-features --features mysql` or `--features redis` fails with `cannot find type ContainerAsync in crate testcontainers`. Lift `ContainerAsync` and `runners::AsyncRunner` imports to `#[cfg(any(feature = "postgres", feature = "mysql", feature = "redis"))]`.

---

## P1 — Major (Fix before production release, multi-tenant deployment, or worker cluster)

> Fix before production release milestone.

### Group B — Worker & Outbox Async Panic Unwinding Isolation (solve together)
> Root cause: Wrapping `std::panic::catch_unwind` around future construction closures rather than the polled asynchronous future itself.

- [x] **Outbox M1 — Unhandled asynchronous dispatcher panics bypass `catch_unwind` and terminate `OutboxRelay` background worker loop** (`outbox-data-integrity.md` M1).
      `rume/src/outbox/relay.rs:113–120`: `std::panic::catch_unwind` is wrapped only around calling the closure that returns the future (`dispatcher.dispatch(&message)`), but NOT around `fut.await`. Panics during async execution unwind uncaught, permanently terminating the background worker task in `start()`. Wrap the async future itself with `std::panic::AssertUnwindSafe(fut).catch_unwind().await`.
- [x] **Jobs M1 — Unhandled asynchronous job handler panics bypass `catch_unwind`, stranding jobs in `in_flight` status without DLQ routing or lock release** (`jobs-worker-concurrency.md` M1, `testing-feature-matrix.md` M2).
      `rume/src/jobs/worker.rs:366–371`: In `execute_job`, `catch_unwind` wraps only the closure constructing the future, while `fut.await` is evaluated outside `catch_unwind`. Unhandled async panics unwind uncaught to task join boundaries, stranding jobs in `status = 'in_flight'` in PostgreSQL and breaking `test_worker_panic_does_not_leak_in_flight_count`. Wrap the future execution itself in `AssertUnwindSafe(fut).catch_unwind().await`.

### Group C — Distributed Coordination Fencing & Idempotency Eviction (solve together)
> Root cause: Overly restrictive state checks in Lua eviction scripts and preserved election terms on unexpired crash re-acquisition.

- [x] **Coordination M1 — `RedisIdempotencyStore::release` ignores `COMPLETED` records, breaking key eviction and failing `test_redis_idempotency_store`** (`coordination-multi-tenant.md` M1, `jobs-worker-concurrency.md` m1, `testing-feature-matrix.md` M2).
      `rume/src/jobs/redis.rs:320–334`: `release` script only deletes keys if value is `'IN_PROGRESS'`. Consequently, `'COMPLETED'` records can never be cleared, reset, or evicted, causing `test_redis_idempotency_store` (`rume/tests/containers/redis.rs:534`) to fail on re-acquisition. Update Lua script to delete keys if status is either `'IN_PROGRESS'` or `'COMPLETED'`.
- [x] **Coordination M2 — `leader_acquire.sql` preserves election term on unexpired re-acquisition, defeating monotonic fencing on node crash recovery** (`coordination-multi-tenant.md` M2).
      `rume/src/persistence/sql/postgres/leader_acquire.sql:6–12`: Query retains `OR rume_leaders.candidate_id = $2` and suppresses term incrementation when `expires_at > clock_timestamp()`. A crashed node restarting within its TTL calls `try_acquire` on boot and re-acquires leadership with the exact same term, defeating downstream split-brain fencing and diverging from `RedisLeaderElector`. Increment `term = nextval('rume_leader_term_seq')` on every acquisition.

### Group D — Security Envelope Signature Binding & CSRF Hardening (solve together)
> Root cause: Assumption that only child `<Assertion>` tags are signed in SAML responses, and insecure opt-in defaults in `CsrfConfig`.

- [x] **Security M1 — SAML 2.0 Signature Reference binding in `SamlParser` rejects standard signed Response envelopes and is fragile to namespace variants** (`security-protocols.md` M1).
      `rume/src/enterprise/saml.rs:263–287`: Parser assumes only child `<Assertion>` tags are referenced by `<ds:Reference URI="#...">`, causing valid SAML responses where the `<Response>` envelope is signed (standard in Okta, Azure AD, and ADFS) to fail with `SignatureWrappingDetected`. Update reference target extraction to support both `<Assertion ID="...">` and `<Response ID="...">` root envelopes.
- [x] **Security M2 — Insecure defaults in `CsrfConfig` persist: missing mandatory HMAC secret and insecure cookies by default** (`security-protocols.md` M2).
      `rume/src/security/csrf.rs:64–77`: Default config sets `secret: None` and `cookie_secure: false`, permitting unauthenticated tokens and subdomain cookie-tossing out of the box unless developers manually configure HMAC secrets. Generate a secure random HMAC secret on startup if none is provided, or make secret configuration mandatory.

### Group E — Container Test Reliability, Docker Bleed & Verification Depth (solve together)
> Root cause: Uncoordinated background worker tasks in reliability tests, Redis container bleed into zero-docker test binaries, and shallow assertions.

- [x] **Jobs M2 — Race condition and uncoordinated worker execution in `test_worker_heartbeat_and_abandoned_job_recovery` causes deterministic container test failure** (`jobs-worker-concurrency.md` M2, `testing-feature-matrix.md` M2).
      `rume/tests/containers/job_reliability.rs:227–258`: `worker1` is started before enqueuing and claiming the fixture job. `worker1`'s background poll loop steals the job immediately, causing `queue.dequeue_for("crashed-worker-99")` to return `None` and panic on `assert!(claimed.is_some())`. Enqueue and claim the job under `crashed-worker-99` *before* starting `worker1`.
- [x] **Testing M1 — Contamination of "zero-docker" `tests/integration.rs` with 15 unshared live Redis container spin-ups** (`testing-feature-matrix.md` M1).
      `rume/tests/integration/`: `auth.rs`, `cache.rs`, `coordination.rs`, and `tenant.rs` spin up 15 separate Docker containers inside the `integration` test binary, violating the zero-docker contract of `tests/integration.rs`. Move Redis container suites to `tests/containers/` and connect to shared `OnceCell` Redis container fixtures.
- [x] **Testing M3 — Potemkin pool lifecycle assertions in `containers/postgres.rs` persist without verifying query rejection or physical connection termination** (`testing-feature-matrix.md` M3).
      `rume/tests/containers/postgres.rs:77–81`: Test checks only boolean flags on `pool` without attempting a query after `close_and_drain` or verifying that physical TCP connections are severed. Assert that queries on closed pools return `Err(PersistenceError::PoolClosed)`.
- [x] **Testing M4 — 16 non-deterministic wall-clock sleeps remain across 6 container test suites** (`testing-feature-matrix.md` M4).
      `rume/tests/containers/{jobs.rs, job_reliability.rs, mysql.rs, postgres.rs, redis.rs}`: 16 wall-clock `sleep()` calls remain. Replace with atomic synchronization (`Notify`, channels) or virtual time.

---

## P2 — Minor Hardening & Operational Polish

> Schedule in follow-up sprint; non-blocking for core reliability milestones.

- [ ] **Outbox m1 — Hardcoded `max_retries` threshold of 5 remains unconfigurable across database drivers** (`outbox-data-integrity.md` m1).
      `rume/src/persistence/{postgres,mysql,sqlite}.rs:374`: Drivers hardcode `retry_count >= 5` in SQL. Add `max_retries: u32` to `OutboxRelayConfig` and driver claim queries.
- [ ] **Pool m1 — `DatabaseConfig::auto_migrate` configuration property is completely disconnected and ignored** (`pool-lifecycle-persistence.md` m1).
      `rume/src/persistence/config.rs:25`: `auto_migrate` field exists but is never read or executed on pool startup.
- [ ] **Jobs m1 — Redis `IdempotencyStore::release` and `complete` perform unverified mutations across expired lease renewals** (`jobs-worker-concurrency.md` m1).
      `rume/src/jobs/redis.rs:284–302, 318–334`: `complete` and `release` Lua scripts check only key presence and status, but not the worker token that acquired the claim.
- [x] **Jobs m2 — Job lease renewal failure in `JobWorker` heartbeat does not signal cancellation to running job handler** (`jobs-worker-concurrency.md` m2).
      `rume/src/jobs/worker.rs:356–361`: If `renew_lease` returns an error, the heartbeat task logs a warning but does not trigger the `CancellationToken` in `JobContext`.
- [x] **Coordination m1 — Duplicate sequence evaluation in `leader_acquire.sql` conflict update path burns term sequence numbers** (`coordination-multi-tenant.md` m1).
      `rume/src/persistence/sql/postgres/leader_acquire.sql:10–12`: Conflict resolution evaluates `nextval()` multiple times on collision.
- [ ] **Coordination m2 — `RateLimitService` ignores `TenantContext` extensions, bypassing tenant rate limiting for subdomain-routed requests** (`coordination-multi-tenant.md` m2).
      `rume/src/resilience/rate_limiter.rs:260–270`: Rate limiting middleware extracts tenant ID only from headers, ignoring `TenantContext` injected by `SubdomainTenantResolver`.
- [ ] **Coordination m3 — `RateLimiter` retains global write lock serialization and lacks background eviction scheduling** (`coordination-multi-tenant.md` m3).
      `rume/src/resilience/rate_limiter.rs:45–80`: In-memory rate limiter acquires exclusive `write()` lock on every check; consider sharding with `DashMap`.
- [x] **Security m1 — Non-atomic session ID rotation in `SessionStore::regenerate_id` risks session state loss on write failure** (`security-protocols.md` m1).
      `rume/src/auth/session.rs:77–84`: `regenerate_id` deletes `old_id` before writing `new_id`. If `save(new)` fails, session data is lost.
- [ ] **Security m2 — Audit logger literal escaping data distortion in Postgres and SQLite** (`security-protocols.md` m2).
      `rume/src/security/audit.rs:188–205`: Unconditional MySQL C-string escaping doubles backslashes and converts newlines to literal `\n` in PostgreSQL and SQLite.
- [ ] **Testing m1 — Redundant test assertions between unit and integration suites in `integration/health.rs` and `integration/error.rs`** (`testing-feature-matrix.md` m1).
      `rume/tests/integration/{health.rs, error.rs}`: Prune duplicate domain variant checks already covered in unit tests.
- [x] **Testing m2 — Six unused import compiler warnings in `rume/src/testing/harness.rs`** (`testing-feature-matrix.md` m2).
      `rume/src/testing/harness.rs:1–10`: Remove unused imports under default feature configurations.

---

## Counts

| Priority Tier | Group Count | Total Findings | Critical (C) | Major (M) | Minor (m) |
|---|---|---|---|---|---|
| **P0 — Critical** | 1 Group (A) | **2** | 2 | 0 | 0 |
| **P1 — Major** | 4 Groups (B–E) | **11** | 0 | 11 | 0 |
| **P2 — Minor** | Ungrouped | **11** | 0 | 0 | 11 |
| **Total** | **5 Groups** | **24** | **2** | **11** | **11** |

*All 24 findings across the 6 diff review reports are strictly accounted for with zero dropped items.*
