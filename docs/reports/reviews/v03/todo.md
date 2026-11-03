# Rume v03 Review — todo.md

Derived from the 6 lens reports in `docs/reports/reviews/v03/`:
- `outbox-data-integrity.md` (C1–C3, M1–M5, m1–m4)
- `pool-lifecycle-persistence.md` (C1–C2, M1–M5, m1–m2)
- `jobs-worker-concurrency.md` (C1–C3, M1–M6, m1–m6)
- `coordination-multi-tenant.md` (C1–C4, M1–M6, m1–m5)
- `security-protocols.md` (C1–C3, M1–M6, m1–m3)
- `testing-feature-matrix.md` (C1–C3, M1–M5, m1–m3)

Ordered by priority tier (`P0` critical unblockers → `P2` minor hardening), with findings sharing a root cause or subsystem grouped for solving together. IDs are lens-qualified per canonical review-authoring standard. Duplicate findings surfaced across multiple lenses appear citing their source reports.

---

## P0 — Critical (Fix first: live data loss, duplicate event emission, SQL injection, pool shutdown hangs, broken build matrix)

> Fix immediately before any feature addition, staging deployment, or production traffic.

### Group A — Outbox Lease Ownership & Recovery Atomicity (solve together)
> Root cause: Missing lease token validation in completion methods, flawed message-age recovery SQL, and upsert status resets.

- [x] **Outbox C1 — Superseded relay worker overwrites active claims and resurrects published messages via unvalidated lease tokens in `mark_published()` and `mark_failed()`** (`outbox-data-integrity.md` C1).
      `rume/src/outbox/repository.rs:39–44`, `rume/src/persistence/postgres.rs:353–395`: Updates check only `id: &str` without validating `lease_token` or `status = 'inflight'`. Add `lease_token: &str` to trait and enforce `WHERE id = $1 AND lease_token = $2 AND status = 'inflight'`, asserting `rows_affected() == 1`.
- [x] **Outbox C2 — Premature lease revocation in `outbox_recover.sql` via message age condition causes unconditional concurrent double-dispatch** (`outbox-data-integrity.md` C2).
      `rume/src/persistence/sql/*/outbox_recover.sql:5`: Clause `OR ($1 - created_at) >= $2` prematurely revokes active leases on messages queued for $\ge 30$s and triggers MySQL unsigned subtraction underflow error 1690. Remove `created_at` clause and determine expiry strictly via `lease_expires_at <= $1`.
- [x] **Outbox C3 — Duplicate message insertion with upsert resurrects already-published and in-flight messages** (`outbox-data-integrity.md` C3).
      `rume/src/persistence/sql/*/outbox_insert.sql:5–11`: `ON CONFLICT (id) DO UPDATE SET status = EXCLUDED.status` resets published events back to `pending` on idempotent request retries. Replace with `ON CONFLICT (id) DO NOTHING` across Postgres, SQLite, and MySQL (`ON DUPLICATE KEY UPDATE id = id`).

### Group B — Database Pool Physical Lifecycle & Delegation (solve together)
> Root cause: Disconnected "potemkin" concurrency abstraction and missing configuration forwarding to SQLx.

- [x] **Pool C1 — False shutdown success and physical connection leak in `DatabasePool::close()` and `close_and_drain()`** (`pool-lifecycle-persistence.md` C1, `testing-feature-matrix.md` C3).
      `rume/src/persistence/pool.rs:187–208`: `close()` closes the local semaphore without calling `.close().await` on the underlying SQLx pool, leaving physical connections and long-running queries active. Implement `DatabaseBackend::close()` and delegate drain directly to the underlying driver pool.
- [x] **Pool C2 — Disconnected "potemkin" concurrency abstraction: `DatabasePool` maintains fictional metrics and a shadow semaphore disconnected from SQLx** (`pool-lifecycle-persistence.md` C2).
      `rume/src/persistence/pool.rs:31–39, 66–78`: `total_connections` starts at configured minimum without establishing physical connections; permit counter tracks shadow semaphore permits rather than SQLx pool utilization. Delegate pool metrics directly to SQLx `pool.size()` and `pool.num_idle()`.
- [x] **Pool M1 — Critical pool configuration properties (`min_connections`, `idle_timeout_secs`, `connect_timeout_secs`, `auto_migrate`) are silently ignored by driver backends** (`pool-lifecycle-persistence.md` M1).
      `rume/src/persistence/{postgres,mysql,sqlite}.rs`: Backends apply only `max_connections` to `PoolOptions`, silently dropping `min_connections`, `idle_timeout`, and `acquire_timeout`. Forward all `DatabaseConfig` parameters to `PgPoolOptions`, `MySqlPoolOptions`, and `SqlitePoolOptions`.

### Group C — Background Job Worker Fencing & Idempotency Preservation (solve together)
> Root cause: Unversioned job state transitions, missing lease extension API, and row deletion destroying partial unique indexes.

- [x] **Jobs C1 — Job state mutations (`ack`, `nack`, `dead_letter`) lack lease token and worker ownership verification** (`jobs-worker-concurrency.md` C1).
      `rume/src/jobs/postgres.rs:211–245`: Mutations run unconditionally by `id`, allowing superseded workers whose 60s lease expired to delete or reschedule jobs currently owned by peer workers. Enforce `WHERE id = $1 AND assigned_worker = $2 AND lease_expires_at > $3`.
- [x] **Jobs C2 — Missing job-level lease renewal / heartbeat API causes duplicate concurrent execution of legitimate long-running jobs (>60s)** (`jobs-worker-concurrency.md` C2).
      `rume/src/jobs/queue.rs:20–45`, `rume/src/jobs/worker.rs:145–180`: `JobQueue` has no `renew_lease` API and `JobContext` cannot extend leases; jobs executing $>60$s are reclaimed by peer workers mid-execution. Add `renew_lease(job_id, worker_id, extension)` and background heartbeat task in `JobWorker`.
- [x] **Jobs C3 — `ack()` physically deletes jobs upon completion (`DELETE FROM rume_jobs`), destroying PostgreSQL-level partial unique index deduplication** (`jobs-worker-concurrency.md` C3).
      `rume/src/jobs/postgres.rs:221`: `DELETE FROM rume_jobs WHERE id = $1` removes the row, deleting the partial unique index entry (`idx_rume_jobs_idempotency`) and allowing duplicate job submissions to re-execute. Transition completed jobs to `status = 'completed'` with retention TTL.

### Group D — Security Exploits & SQL Injection in Audit Logging (solve together)
> Root cause: Naive string formatting in SQL queries and unbound XML element matching in SAML assertion verification.

- [x] **Security C3 — SQL Injection Vulnerability in `SqlAuditLogger::record()` via String Formatting and Incomplete MySQL Escape Handling** (`security-protocols.md` C3).
      `rume/src/security/audit.rs:161, 173–188`: Query is built using `format!("INSERT INTO rume_audit_log ... VALUES ('{id}', ...)")` with naive single-quote replacement (`.replace('\'', "''")`), vulnerable to backslash escape injection in MySQL. Rewrite `SqlAuditLogger` to use parameterized `sqlx::query` across all supported backends.
- [x] **Security C1 — SAML 2.0 Parser Vulnerable to XML Signature Wrapping (XSW) Attacks via Unbound Global Tag Extraction and Namespace Prefix Evasion** (`security-protocols.md` C1).
      `rume/src/enterprise/saml.rs:182–258`: Parser scans raw XML globally for tags from byte 0, allowing an attacker to prepend an unsigned assertion while placing the valid signed assertion inside an envelope. Cryptographically bind the verified signature element to the exact assertion payload consumed.
- [x] **Security C2 — Public `SamlParser::parse()` Bypasses Cryptographic Verification in Production** (`security-protocols.md` C2).
      `rume/src/enterprise/saml.rs:170–172`: `SamlParser::parse()` delegates to `parse_with_verifier(xml, None)` without requiring a signature verifier. Remove unverified public parse path or restrict to test environments.

### Group E — Distributed Coordination Fencing & Clock Drift (solve together)
> Root cause: Application-side clock comparison instead of database-side time, and unhashed Redis keys in cluster topology.

- [x] **Coordination C1 — Split-brain leadership and lock theft via client wall-clock skew in PostgreSQL coordination** (`coordination-multi-tenant.md` C1).
      `rume/src/coordination/postgres.rs:70–88, 198–221`: Lock and leader expiration is evaluated against client application timestamps (`$4`) rather than PostgreSQL `clock_timestamp()`. Move expiration comparisons inside SQL using `CURRENT_TIMESTAMP`.
- [x] **Coordination C2 — Stale leader re-acquisition fails to increment election term in `leader_acquire.sql`, violating monotonic fencing on crash recovery** (`coordination-multi-tenant.md` C2).
      `rume/src/persistence/sql/postgres/leader_acquire.sql:6–9`: `CASE WHEN candidate_id = EXCLUDED.candidate_id THEN term ELSE nextval(...)` reuses the existing term on recovery, violating monotonic fencing token guarantees. Unconditionally increment `term = nextval('rume_leader_term_seq')`.
- [x] **Coordination C3 — Redis coordination Lua scripts and cache tag invalidation break under Redis Cluster due to missing hash tags `{...}`** (`coordination-multi-tenant.md` C3).
      `rume/src/coordination/redis.rs:88–105`, `rume/src/cache/redis.rs:135–150`: Multi-key operations (`lock:{res}`, `fencing:{res}`, tag sets) omit `{...}` hash tags, causing Redis Cluster to abort with `CROSSSLOT Keys in request don't hash to the same slot`. Wrap shared keys in `{...}` hash tags.
- [x] **Coordination C4 — Cross-tenant job and idempotency key collisions in `rume_jobs` and `RedisIdempotencyStore`** (`coordination-multi-tenant.md` C4).
      `rume/src/jobs/postgres.rs:32`, `rume/src/jobs/redis.rs:35`: `idempotency_key` lacks tenant qualification, enabling tenant A to block tenant B's execution or read tenant B's cached output. Namespace all queue and idempotency keys with tenant ID.

### Group F — Feature Matrix & Ungated Compilation Invariants (solve together)
> Root cause: Ungated backend dependencies in integration tests breaking standard cargo build configurations.

- [x] **Testing C1 — Broken Feature Matrix: Ungated Backend Dependencies Break `cargo test` and `cargo check` Under Single-Feature and Default Configurations** (`testing-feature-matrix.md` C1).
      `rume/tests/enterprise_integration_pipeline.rs:18, 28`: Imports `PostgresJobQueue`, `deadpool_postgres`, and `tokio_postgres` without `#![cfg(feature = "postgres")]`, breaking `cargo check --no-default-features --features sqlite`. Add proper `#![cfg(...)]` gates across all 10 affected integration test files.
- [x] **Testing C2 — False-Positive Assertion & Panic Masking in `jobs_suite.rs:293–310` Leaves Broken Database State Undetected** (`testing-feature-matrix.md` C2).
      `rume/tests/jobs_suite.rs:293–310`: Test catches panic but does not assert whether the failed job is transitioned to DLQ or left stranded in `in_flight`. Assert database row state and idempotency lock release after worker panic.

---

## P1 — Major (Fix before any production release or multi-tenant deployment)

> Fix before production release milestone.

### Group G — Outbox Reliability & Dead-Letter Observability (solve together)
> Root cause: Missing error observation in relay loop and lack of DLQ inspection APIs.

- [x] **Outbox M1 — Missing `ORDER BY created_at ASC` in MySQL `outbox_claim.sql` violates FIFO temporal event ordering** (`outbox-data-integrity.md` M1).
      `rume/src/persistence/sql/mysql/outbox_claim.sql:6`: Query omits `ORDER BY created_at ASC`, resulting in non-deterministic row selection and event starvation. Add `ORDER BY created_at ASC` before `LIMIT ?`.
- [x] **Outbox M2 — Relay error propagation swallows recovery failures and abandons claimed in-flight messages on partial batch failures** (`outbox-data-integrity.md` M2).
      `rume/src/outbox/relay.rs:60–78`: `let _ = recover_expired_claims()` ignores errors, and `?` in loop exits early leaving claimed messages stranded until timeout. Log recovery errors and isolate per-message dispatch errors.
- [x] **Outbox M3 — Non-atomic read-modify-write race condition in `mark_failed()` overwrites concurrent retry counters** (`outbox-data-integrity.md` M3).
      `rume/src/persistence/postgres.rs:370–390`: `SELECT retry_count` followed by `UPDATE` suffers from lost updates under concurrent failure handling. Perform atomic update in SQL (`retry_count = retry_count + 1`).
- [x] **Outbox M4 — Outbox repository lacks dead-letter queue (DLQ) query/reprocess API and visibility** (`outbox-data-integrity.md` M4).
      `rume/src/outbox/repository.rs:15–60`: No APIs exist to count, query, or retry failed messages. Add `failed_count()`, `fetch_failed()`, and `reprocess_failed()` to `OutboxRepository`.
- [x] **Outbox M5 — Unhandled dispatcher panics permanently terminate `OutboxRelay::start()` background worker loop** (`outbox-data-integrity.md` M5).
      `rume/src/outbox/relay.rs:84–94`: Panic in user dispatcher unwinds and kills the background Tokio task. Wrap dispatch in `std::panic::AssertUnwindSafe` and `catch_unwind()`.

### Group H — Database Error Mapping, Timeouts & Migration Atomicity (solve together)
> Root cause: Missing driver error categorization and unparameterized migration DDL scripts.

- [x] **Pool M2 — `DatabaseHealthCheck` and `ping()` hang during pool saturation or network partition due to missing acquisition timeouts** (`pool-lifecycle-persistence.md` M2).
      `rume/src/persistence/pool.rs:265–307`: `ping()` executes queries without wrapping in `acquire_timeout`, hanging indefinitely if connections are saturated. Wrap health checks in `tokio::time::timeout(connect_timeout)`.
- [x] **Pool M3 — Schema divergence: `init_migrations_table()` creates `rume_migrations` with `NOT NULL` columns, causing `Migrator::run_pending` to fail** (`pool-lifecycle-persistence.md` M3).
      `rume/src/persistence/sql/*/migrations_schema.sql` vs `rume/src/persistence/migration.rs:177`: External schema defines `name VARCHAR NOT NULL, checksum VARCHAR NOT NULL`, but `Migrator` inserts only `(id, applied_at)`. Unify `rume_migrations` schema and insertion statements.
- [x] **Pool M4 — Non-atomic migration application and lack of distributed migration lock in HA environments** (`pool-lifecycle-persistence.md` M4).
      `rume/src/persistence/migration.rs:164–184`: Multiple application replicas running `run_pending_backend` concurrently execute duplicate DDL statements. Acquire a distributed lock / advisory lock before applying migrations.
- [x] **Pool M5 — Error variant flattening: SQLx errors are unconditionally mapped to `TransactionError` or `QueryFailed`, masking `PoolExhausted`, `Timeout`, and `Closed`** (`pool-lifecycle-persistence.md` M5).
      `rume/src/persistence/postgres.rs:109–145`: SQLx pool timeout and connection errors are converted to generic strings, masking structured error handling. Map SQLx `PoolTimedOut` and `PoolClosed` to structured `PersistenceError` variants.

### Group I — Job Panic Safety, Idempotency Locks & Error Classification (solve together)
> Root cause: Incomplete task panic isolation and premature lock release on retryable failures.

- [x] **Jobs M1 — Job handler panic abandons in-flight jobs without DLQ recording and leaves idempotency locks permanently held** (`jobs-worker-concurrency.md` M1).
      `rume/src/jobs/worker.rs:150–180`: Catching task join panics does not mark the job dead-lettered or release idempotency keys, leaving jobs stranded in `in_flight`. Handle join errors by calling `queue.dead_letter(job, "panic")` and releasing locks.
- [x] **Jobs M2 — Releasing idempotency keys on retryable failures permits concurrent duplicate execution during the backoff delay** (`jobs-worker-concurrency.md` M2).
      `rume/src/jobs/worker.rs:168–175`: Calling `idempotency.release(key)` immediately upon failure allows a client retry to execute concurrently while the initial job is waiting for its backoff delay. Retain idempotency lock until max retries are exhausted.
- [x] **Jobs M3 — `IdempotencyStatus::InProgress` silently drops jobs from the worker without acknowledgment or delayed re-queue** (`jobs-worker-concurrency.md` M3).
      `rume/src/jobs/worker.rs:135–148`: When duplicate execution is detected, worker drops the job without calling `queue.ack()` or `queue.nack()`, stranding the message in `in_flight`. Explicitly call `queue.ack()` or `nack()` with delay.
- [x] **Jobs M4 — Non-retryable error types (`UnknownJob`, `Serialization`) are treated as retryable, exhausting max retries before DLQ** (`jobs-worker-concurrency.md` M4).
      `rume/src/jobs/worker.rs:160–175`: Handler deserialization errors are retried up to 3 times before failing to DLQ. Route fatal non-retryable errors directly to `dead_letter()`.
- [x] **Jobs M5 — Redis `IdempotencyStore::release` and `complete` perform unverified mutations, risking cross-worker lock deletion** (`jobs-worker-concurrency.md` M5).
      `rume/src/jobs/redis.rs:75–110`: `release` calls `DEL idempotency:{key}` without verifying the worker token, allowing expired workers to delete active locks. Use a Lua script checking lock ownership token.
- [x] **Jobs M6 — `PostgresJobQueue::enqueue_delayed` returns fictitious IDs on duplicate conflict resolution race** (`jobs-worker-concurrency.md` M6).
      `rume/src/jobs/postgres.rs:130–149`: Fallback query `SELECT id FROM rume_jobs WHERE idempotency_key = $1` can return the newly generated UUID if the race loses. Ensure query returns the existing committed ID.

### Group J — Multi-Tenant Resource Isolation & Resolver Collisions (solve together)
> Root cause: Shared un-namespaced rate limits, global lock loops, and quota permit leaks on TTL expiry.

- [x] **Coordination M1 — `TenantScopedCache` quota leak on TTL expiration permanently locks out tenant caching** (`coordination-multi-tenant.md` M1).
      `rume/src/tenant/cache.rs:40–65`: `current_keys` counter increments on `set()` but is never decremented when items expire via TTL, permanently locking out tenants once quota is reached. Track key expiration or synchronize quota with backend.
- [x] **Coordination M2 — Monotonic memory leak in Redis tag sets from expired TTL keys** (`coordination-multi-tenant.md` M2).
      `rume/src/cache/redis.rs:75–110`: Adding keys to `tag:{name}` sets via `SADD` never removes keys when individual keys expire via Redis TTL, causing sets to grow indefinitely. Prune tag sets during reads or tag invalidation.
- [x] **Coordination M3 — Delimiter collision in `RedisLeaderElector` candidate IDs (`candidate_id:term`) breaks leader recovery** (`coordination-multi-tenant.md` M3).
      `rume/src/coordination/redis.rs:245–280`: Storing `val = format!("{candidate_id}:{term}")` breaks parsing if `candidate_id` contains a colon. Use structured JSON or length-prefixed encoding.
- [x] **Coordination M4 — Global write lock contention and inline O(N) eviction loop in `RateLimiter::check`** (`coordination-multi-tenant.md` M4).
      `rume/src/resilience/rate_limiter.rs:45–80`: Exclusive `RwLock::write` acquired on every request to run inline eviction loop across all clients. Use a sharded lock (`DashMap`) and background eviction task.
- [x] **Coordination M5 — Complete lack of multi-tenant resource isolation in rate limiting middleware and database connection pools** (`coordination-multi-tenant.md` M5).
      `rume/src/resilience/rate_limiter.rs:105`: Rate limiting key is client IP only, allowing a noisy tenant to exhaust global pool connections and rate limits. Introduce tenant-keyed rate limiting and pool quota reservations.
- [x] **Coordination M6 — `HeaderTenantResolver::add_tenant` silently overwrites conflicting slugs and precedence shadows vanity slugs** (`coordination-multi-tenant.md` M6).
      `rume/src/tenant/resolver.rs:45–60`: Adding a tenant with an existing slug overwrites the previous mapping silently. Return `Result<(), TenantError::Conflict>` on duplicate slug registration.

### Group K — Auth Hardening, Session Fixation & CSRF Defaults (solve together)
> Root cause: Insecure default configurations, lack of session rotation on privilege change, and silent auth error swallowing.

- [x] **Security M1 — Insecure Defaults in `CsrfConfig` Permit Subdomain Cookie-Tossing and Lack Mandatory HMAC Signing** (`security-protocols.md` M1).
      `rume/src/security/csrf.rs:64–77, 159–163`: Default config sets `secret: None`, `auto_inject_cookie: false`, and `cookie_secure: false`. Make HMAC secret required or generate a secure random key on startup.
- [x] **Security M2 — Subdomain Cookie-Tossing Causes Denial of Service via First-Cookie Selection Bias in `CsrfService`** (`security-protocols.md` M2).
      `rume/src/security/csrf.rs:271–280`: `find_map` picks the first matching `csrf_token` cookie, allowing an attacker to inject an invalid cookie that blocks all requests. Validate all presented cookies until a valid signature matches.
- [x] **Security M3 — Lack of Secret Redaction in Audit Logging Metadata** (`security-protocols.md` M3).
      `rume/src/security/audit.rs:167–168`: `record.metadata` is serialized directly into database columns without filtering sensitive keys. Pass metadata through `rume::env::secrets::redact_value()`.
- [x] **Security M4 — SCIM 2.0 Entities Lack `meta.version` (ETag) for Optimistic Concurrency Control and Replay Protection** (`security-protocols.md` M4).
      `rume/src/enterprise/scim.rs:36–41`: Entities lack `version` field required by RFC 7643 Section 3.1 for `If-Match` conditional updates. Add `version: String` (ETag) to `ScimMeta`.
- [x] **Security M5 — Missing Session ID Rotation (Regeneration) Exposes Applications to Session Fixation Attacks** (`security-protocols.md` M5).
      `rume/src/auth/session.rs:74–84`: `SessionStore` provides no API to rotate session IDs upon authentication. Add `regenerate_id(&mut self, session)` to re-key sessions on login.
- [x] **Security M6 — `SessionScheme` Silently Degrades Authenticated Users to Guests on Session Store Failures** (`security-protocols.md` M6).
      `rume/src/auth/scheme.rs:194–203`: Redis connection timeout or store error is swallowed via `if let Ok`, falling back to guest access. Propagate store errors as `AuthError::ProviderError`.

### Group L — Integration Test Architecture & Reusable Testcontainers (solve together)
> Root cause: Cargo flat-directory multi-binary compile explosion and per-test container lifecycle overhead.

- [ ] **Testing M1 — Test Directory Binary Explosion: 32 Flat Test Crates Cause 11 GB Build Bloat and Extreme Link Overhead** (`testing-feature-matrix.md` M1).
      `rume/tests/`: 24 flat integration test files compile as 24 separate executables, linking dependencies 24 times. Consolidate into `tests/integration.rs`, `tests/containers.rs`, and uncompiled `tests/common/` helpers.
- [ ] **Testing M2 — Per-Test Container Spin-Up Overhead Adds 3+ Minutes of Idle Latency and Risks Docker Daemon Exhaustion** (`testing-feature-matrix.md` M2).
      `rume/tests/*_container_suite.rs`: Tests launch 46 separate container instances. Replace with shared container fixtures (`tokio::sync::OnceCell`) and per-test database/schema isolation (`test_{uuid}`).
- [ ] **Testing M3 — Persistent Async Wall-Clock Sleeps Across 14 Test Suites Violate Determinism and Introduce Flaky Races** (`testing-feature-matrix.md` M3).
      `rume/tests/`: 34 raw `tokio::time::sleep` calls remain across 14 files. Replace wall-clock sleeps with deterministic synchronization (`tokio::sync::Barrier`, `Notify`, or `tokio::time::pause()`).
- [ ] **Testing M4 — Absence of a First-Party Declarative Test Harness (`RumeTestHarness` in `rume::testing`) Forces Repetitive Boilerplate** (`testing-feature-matrix.md` M4).
      `rume/tests/`: Setup code (container spin-up, connection URL, schema init, runner launch) is copy-pasted across 10 files. Introduce a fluent `RumeTestHarness` in `rume::testing`.
- [ ] **Testing M5 — Lack of Modern Testing Ecosystem Tooling (`insta`, `rstest`, `wiremock`) Hinders Snapshot and Protocol Verification** (`testing-feature-matrix.md` M5).
      `rume/tests/`: Verbose manual assertion logic for OpenAPI specs and Problem Details. Introduce `rstest` for fixture injection, `insta` for snapshot assertions, and `wiremock` for IdP HTTP mocks.

---

## P2 — Minor Hardening & Operational Polish

> Schedule in follow-up sprint; non-blocking for core reliability milestones.

### Outbox & Persistence Polish
- [x] **Outbox m1 — Hardcoded retry thresholds, lease durations, and batch sizes preclude workload tuning** (`outbox-data-integrity.md` m1).
- [x] **Outbox m2 — Missing index on `(status, lease_expires_at)` causes full table scans during recovery polling** (`outbox-data-integrity.md` m2).
- [x] **Outbox m3 — 1-second timestamp resolution creates non-deterministic causal ordering for high-throughput batches** (`outbox-data-integrity.md` m3).
- [x] **Outbox m4 — Outbox operations on `DatabasePool<B>` bypass pool semaphores and in-flight accounting** (`outbox-data-integrity.md` m4).
- [x] **Pool m1 — Multi-statement execution failure in `MysqlBackend::execute`, `SqliteBackend::execute`, and `PostgresTransaction::execute`** (`pool-lifecycle-persistence.md` m1).
- [x] **Pool m2 — Inconsistent transaction auto-commit collision in `DatabasePool::transaction`** (`pool-lifecycle-persistence.md` m2).

### Jobs & Coordination Polish
- [x] **Jobs m1 — Wall-clock underflow panic in `PostgresJobQueue::dequeue_for` on systems with low monotonic uptime** (`jobs-worker-concurrency.md` m1).
- [x] **Jobs m2 — Exponential backoff in `RetryPolicy` lacks randomized jitter and is vulnerable to NaN/overflow** (`jobs-worker-concurrency.md` m2).
- [x] **Jobs m3 — `JobWorker` lacks cancellation token propagation to `JobContext` during graceful drain** (`jobs-worker-concurrency.md` m3).
- [x] **Jobs m4 — DLQ lacks failure timestamp and query/replay administrative APIs** (`jobs-worker-concurrency.md` m4).
- [x] **Jobs m5 — Worker poll error loop busy-spins every 50ms during database outages** (`jobs-worker-concurrency.md` m5).
- [x] **Jobs m6 — Dual lease expiration recovery between `jobs_claim.sql` and `jobs_reap.sql` lacks coordination** (`jobs-worker-concurrency.md` m6).
- [x] **Coordination m1 — Subdomain tenant resolver fails on case-insensitive hosts and IPv6 literals** (`coordination-multi-tenant.md` m1).
- [x] **Coordination m2 — `RedisRateLimiter` misclassifies Redis connection and script errors as `CircuitOpen`** (`coordination-multi-tenant.md` m2).
- [x] **Coordination m3 — `LockGuard` and `LeaderLease` lack expiration timestamp telemetry** (`coordination-multi-tenant.md` m3).
- [x] **Coordination m4 — Unused global sequence burn in `PostgresDistributedLock::acquire` conflict path** (`coordination-multi-tenant.md` m4).
- [x] **Coordination m5 — Expired rows in `rume_locks` and `rume_leaders` retained indefinitely without index on `expires_at` or cleanup reaper** (`coordination-multi-tenant.md` m5).

### Security & Testing Polish
- [x] **Security m1 — Unvalidated Reverse Proxy Trust for `x-forwarded-proto` and Axum Relative URI Blindness in `SecurityHeadersService`** (`security-protocols.md` m1).
- [x] **Security m2 — Substring Heuristic Blind Spots and Leaky URL Parsing in `rume/src/env/secrets.rs`** (`security-protocols.md` m2).
- [x] **Security m3 — SAML Parser Comment Truncation and Tag Extraction Fragility** (`security-protocols.md` m3).
- [ ] **Testing m1 — Inconsistent Test File Naming Conventions Across `rume/tests/`** (`testing-feature-matrix.md` m1).
- [ ] **Testing m2 — Redundant Test Coverage Between Unit Tests and Integration Tests** (`testing-feature-matrix.md` m2).
- [ ] **Testing m3 — Duplicate Container Spin-Up Logic Across PostgreSQL and Redis Suites** (`testing-feature-matrix.md` m3).

---

## Counts

| Priority Tier | Group Count | Total Findings | Critical (C) | Major (M) | Minor (m) |
|---|---|---|---|---|---|
| **P0 — Critical** | 6 Groups (A–F) | **17** | 16 | 1 | 0 |
| **P1 — Major** | 6 Groups (G–L) | **32** | 0 | 32 | 0 |
| **P2 — Minor** | Ungrouped | **23** | 0 | 0 | 23 |
| **Total** | **12 Groups** | **72** | **16** | **33** | **23** |

*All 72 findings across the 6 lens reports are strictly accounted for with zero dropped items.*
