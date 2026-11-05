# Lens Plan — Diff Review of v03 (`v03/diff/v01`)

## Review Overview
- **Mode:** `diff`
- **Baseline:** `docs/reports/reviews/v03/` (commit `cf82559`)
- **Review Subject:** Changes between `cf82559` and `HEAD` (commits `cf82559..HEAD`, 103 files changed, +3,178 / -750 lines)
- **Review Folder:** `docs/reports/reviews/v03/diff/v01/`
- **Goal:** Verify that all 72 findings from the v03 review are completely and correctly remediated without regressions, that `#![forbid(unsafe_code)]` remains intact, and that the new persistence, concurrency, security, and testing architectures satisfy production invariants.

## Mandatory Context
Each reviewer must read:
1. The corresponding v03 baseline report in `docs/reports/reviews/v03/<lens>.md`.
2. `docs/reports/reviews/v03/todo.md`.
3. The git diff for their subsystem: `git diff cf82559..HEAD -- <paths>`.
4. The full current implementation and test files for their subsystem.

Per `.opencode/skills/review-authoring/SKILL.md`, each report must include a `## Prior decisions` section covering every prior finding in that lens (`REAFFIRMED`, `RESOLVED`, or `NOTE-NEW`).

---

## Staged Execution Graph

### Stage 1: Core Storage & State Engine (Parallel)
Run 2 reviewer subagents in parallel to audit the underlying database and outbox foundations:

1. **`outbox-data-integrity`**
   - **Output:** `docs/reports/reviews/v03/diff/v01/outbox-data-integrity.md`
   - **Scope:**
     - `crates/core/src/outbox/` (`relay.rs`, `repository.rs`, `mod.rs`)
     - SQL schemas & queries: `crates/core/src/persistence/sql/*/outbox_*.sql`
     - Driver implementations in `crates/core/src/persistence/{postgres,mysql,sqlite}.rs`
     - Tests in `crates/core/tests/integration/outbox.rs`
   - **Focus:**
     - Lease token enforcement in `mark_published` and `mark_failed`.
     - Removal of message age `created_at` clause from `outbox_recover.sql`.
     - `ON CONFLICT (id) DO NOTHING` in `outbox_insert.sql`.
     - DLQ query/replay methods (`failed_count`, `fetch_failed`, `reprocess_failed`).
     - Safe panic isolation in `OutboxRelay::start()`.
     - Admission lease acquisition via `pool.acquire_lease()`.

2. **`pool-lifecycle-persistence`**
   - **Output:** `docs/reports/reviews/v03/diff/v01/pool-lifecycle-persistence.md`
   - **Scope:**
     - `crates/core/src/persistence/` (`pool.rs`, `backend.rs`, `migration.rs`, `postgres.rs`, `mysql.rs`, `sqlite.rs`)
     - SQL schemas: `crates/core/src/persistence/sql/*/migrations_schema.sql`
     - Tests in `crates/core/tests/integration/persistence.rs` and `crates/core/tests/containers/{postgres,mysql}.rs`
   - **Focus:**
     - Physical connection pool delegation on `close()` and `close_and_drain()`.
     - Forwarding of `min_connections`, `idle_timeout_secs`, `connect_timeout_secs` to SQLx `PoolOptions`.
     - Structured error variant mapping (`PoolExhausted`, `PoolClosed`, `Timeout`).
     - Multi-statement SQL execution in transaction handles and drivers.
     - Unification of `rume_migrations` schema and insertion statements.
     - Idempotent transaction commit handling.

---

### Stage 2: Concurrency, Coordination & Security Protocols (Parallel)
Run 3 reviewer subagents in parallel:

3. **`jobs-worker-concurrency`**
   - **Output:** `docs/reports/reviews/v03/diff/v01/jobs-worker-concurrency.md`
   - **Scope:**
     - `crates/core/src/jobs/` (`queue.rs`, `worker.rs`, `postgres.rs`, `redis.rs`, `job.rs`, `retry.rs`)
     - SQL queries: `crates/core/src/persistence/sql/postgres/jobs_*.sql`
     - Tests in `crates/core/tests/containers/jobs.rs` and `crates/core/tests/containers/job_reliability.rs`
   - **Focus:**
     - Worker token and lease expiration check in `ack()`, `nack()`, `dead_letter()`.
     - `renew_lease` API and background heartbeat task in `JobWorker`.
     - Retention of completed jobs with TTL (no physical `DELETE` destroying partial unique index).
     - Cancellation token propagation to `JobContext`.
     - Panic safety in worker tasks with DLQ routing.
     - DLQ query and replay administrative APIs (`list_dead_letters`, `replay_dead_letter`).
     - Worker poll backoff on consecutive database errors.

4. **`coordination-multi-tenant`**
   - **Output:** `docs/reports/reviews/v03/diff/v01/coordination-multi-tenant.md`
   - **Scope:**
     - `crates/core/src/coordination/` (`postgres.rs`, `redis.rs`, `lock.rs`, `leader.rs`)
     - `crates/core/src/tenant/` (`cache.rs`, `quota.rs`, `resolver.rs`, `error.rs`)
     - `crates/core/src/resilience/` (`rate_limiter.rs`, `redis.rs`, `error.rs`)
     - SQL queries: `crates/core/src/persistence/sql/postgres/leader_*.sql`, `lock_*.sql`
     - Tests in `crates/core/tests/integration/{coordination,tenant}.rs` and `crates/core/tests/containers/redis.rs`
   - **Focus:**
     - Server-side timestamps (`CURRENT_TIMESTAMP`) in Postgres lock/leader SQL.
     - Unconditional monotonic term increment in `leader_acquire.sql`.
     - Redis Cluster `{...}` hash tags in coordination and cache keys.
     - Tenant-scoped queue and idempotency keys (`tenant_id:idempotency_key`).
     - Prevention of quota leaks on TTL expiry in `TenantScopedCache`.
     - Conflict detection on duplicate slug registration in `HeaderTenantResolver`.
     - Pruning of expired rows via `prune_expired()`.

5. **`security-protocols`**
   - **Output:** `docs/reports/reviews/v03/diff/v01/security-protocols.md`
   - **Scope:**
     - `crates/core/src/security/` (`audit.rs`, `csrf.rs`, `headers.rs`)
     - `crates/core/src/enterprise/` (`saml.rs`, `scim.rs`)
     - `crates/core/src/auth/` (`schemes.rs`, `session.rs`)
     - `crates/core/src/env/` (`secrets.rs`)
     - Tests in `crates/core/tests/integration/{security,enterprise}.rs`
   - **Focus:**
     - Parameterized SQL queries in `SqlAuditLogger` (eliminating string formatting SQL injection).
     - SAML XSW attack mitigation via strict subtree assertion extraction and cryptographic binding.
     - Mandating signature verifiers in `parse_signed` and removing unverified public parse from production.
     - Secure defaults in `CsrfConfig` (mandatory HMAC secret, secure cookies) and multi-cookie traversal.
     - SCIM 2.0 `meta.version` (ETag) optimistic concurrency support.
     - Session ID rotation API `regenerate_id()`.
     - Secret redaction in audit logging metadata and expanded secret patterns.

---

### Stage 3: Testing & Feature Matrix Verification (Sequential)
Run 1 reviewer subagent to audit the consolidated test suite and cargo build matrix:

6. **`testing-feature-matrix`**
   - **Output:** `docs/reports/reviews/v03/diff/v01/testing-feature-matrix.md`
   - **Scope:**
     - `crates/core/Cargo.toml` and root `Cargo.toml`
     - `crates/core/tests/` (`integration.rs`, `containers.rs`, `common/`, `integration/`, `containers/`)
   - **Focus:**
     - Consolidation from 32 flat binaries down to 2 (`integration` and `containers`), eliminating the 11 GB build bloat.
     - Feature gate consistency across single-feature and multi-feature permutations (`sqlite`, `postgres`, `mysql`, `redis`, `no-default-features`).
     - Shared container fixtures (`OnceCell`) reducing container launches from 46 to 3.
     - Total elimination of non-deterministic wall-clock sleeps (`Notify`, virtual time, `yield_now`).
     - Modern ecosystem tooling adoption (`insta`, `rstest`, `wiremock`, `ureq`).
     - Pruning of redundant unit test assertions from integration suites.
     - Strict preservation of `#![forbid(unsafe_code)]` in production code.

---

## Deliverables
- Six lens reports under `docs/reports/reviews/v03/diff/v01/`:
  - `outbox-data-integrity.md`
  - `pool-lifecycle-persistence.md`
  - `jobs-worker-concurrency.md`
  - `coordination-multi-tenant.md`
  - `security-protocols.md`
  - `testing-feature-matrix.md`
- Conductor's `todo.md` (if any actionable findings remain).
