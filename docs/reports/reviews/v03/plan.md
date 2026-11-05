# Lens plan — Rume Whole-Document Review (v03)

Review mode: **whole-document** (confirmed).
This is the third comprehensive review of the repository, staged under `docs/reports/reviews/v03/`.
The plan governs the review only; it never outranks the code (`.opencode/rules/20-doc-tree.md`).

---

## 1. Review Scope & Strategic Context

**Context**: Following the Phase 5 implementation and the SQLx 0.8.6 persistence modernization (commit `819a857`), an external static review of snapshot `pack(7)` identified critical gaps in outbox lease ownership, pool abstraction lifecycle, job worker fencing, feature gate isolation, and SAML cryptographic binding. This review audits the entire codebase against these findings while actively hunting for additional edge-case defects across all subsystems.

**In Scope**:

| Target | Subsystems & Source Files |
|---|---|
| `rume` (Core Library) | `src/persistence/` (`backend.rs`, `config.rs`, `error.rs`, `migration.rs`, `pool.rs`, `postgres.rs`, `mysql.rs`, `sqlite.rs`, `sql/`)<br>`src/outbox/` (`message.rs`, `relay.rs`, `repository.rs`, `error.rs`, `mod.rs`)<br>`src/jobs/` (`context.rs`, `error.rs`, `job.rs`, `queue.rs`, `registry.rs`, `reliability.rs`, `retry.rs`, `worker.rs`, `postgres.rs`, `redis.rs`)<br>`src/coordination/` (`error.rs`, `leader.rs`, `lock.rs`, `postgres.rs`, `redis.rs`)<br>`src/app/` (`builder.rs`, `context.rs`, `lifecycle.rs`, `observability.rs`, `paths.rs`, `shutdown.rs`, `state.rs`, `mode.rs`)<br>`src/auth/` (`identity.rs`, `layer.rs`, `policy.rs`, `scheme.rs`, `session.rs`)<br>`src/cache/` (`backend.rs`, `config.rs`, `error.rs`, `typed.rs`, `redis.rs`)<br>`src/enterprise/` (`saml.rs`, `scim.rs`)<br>`src/env/` (`error.rs`, `interpolation.rs`, `loading.rs`, `secrets.rs`)<br>`src/metrics/` (`layer.rs`, `registry.rs`)<br>`src/module/` (`architecture.rs`, `context.rs`, `dependency.rs`, `lifecycle.rs`, `traits.rs`)<br>`src/resilience/` (`circuit_breaker.rs`, `rate_limiter.rs`)<br>`src/routing/` (`group.rs`, `health.rs`, `matcher.rs`, `registry.rs`, `rejection.rs`, `router.rs`, `spec.rs`, `validated_param.rs`)<br>`src/security/` (`audit.rs`, `csrf.rs`, `headers.rs`)<br>`src/tenant/` (`cache.rs`, `layer.rs`, `model.rs`, `resolver.rs`) |
| `rume-cli` (CLI Tooling) | `src/main.rs`, `cli.rs`, `prompts.rs`, `commands/`, `ui/` |
| Integration Tests | `crates/core/tests/` (All 24 test suites including live Testcontainers suites for PostgreSQL, MySQL, Redis, and SQLite) |

**Out of Scope**:
- `lab/` (experimental scratch space).
- `target/` and generated build artifacts.
- `.opencode/` (review and prompt harness wiring).

---

## 2. Continuity Requirements (from v02)

This review builds upon the findings of `docs/reports/reviews/v02/`:
- `docs/reports/reviews/v02/todo.md` (all 31 items resolved and verified in code)
- `docs/reports/reviews/v02/correctness.md`
- `docs/reports/reviews/v02/security.md`
- `docs/reports/reviews/v02/architecture.md`
- `docs/reports/reviews/v02/enterprise-reliability.md`
- `docs/reports/reviews/v02/dx-tooling.md`
- `docs/reports/reviews/v02/testing.md`

Every reviewer subagent must inspect its lens's prior findings and explicitly include a `## Prior decisions` section declaring:
- **`REAFFIRMED`**: The prior decision remains valid and the fix is verified in code.
- **`CHALLENGED`**: The prior decision is disputed or the fix introduced an unintended regression/contradiction.
- **`NOTE-NEW`**: Newly introduced capabilities, SQLx modernizations, or persistent backends not covered in `v02`.

---

## 3. Lenses & Execution Graph

The review is structured as a **2-stage execution graph** (≤ 3 stages):

```mermaid
flowchart TD
    subgraph Stage 1: Core State, Persistence & Concurrency
        L1["1. Outbox & Data Integrity<br>(outbox-data-integrity.md)"]
        L2["2. Persistence & Pool Lifecycle<br>(pool-lifecycle-persistence.md)"]
        L3["3. Jobs & Worker Concurrency<br>(jobs-worker-concurrency.md)"]
    end

    subgraph Stage 2: Security, Protocols & Ecosystem Verification
        L4["4. Distributed Coordination & Multi-Tenancy<br>(coordination-multi-tenant.md)"]
        L5["5. Security, SAML & Protocol Hardening<br>(security-protocols.md)"]
        L6["6. Testing & Feature Matrix Verification<br>(testing-feature-matrix.md)"]
    end

    L1 --> L4
    L1 --> L5
    L1 --> L6
    L2 --> L4
    L2 --> L5
    L2 --> L6
    L3 --> L4
    L3 --> L5
    L3 --> L6
```

---

## 4. Lens Charters & Deliverables

### Stage 1: Core State, Persistence & Concurrency

#### Lens 1: Outbox & Data Integrity
- **Output File**: `docs/reports/reviews/v03/outbox-data-integrity.md`
- **Lead Subject**: `crates/core/src/outbox/`, `crates/core/src/persistence/{postgres,mysql,sqlite}.rs`, `crates/core/src/persistence/sql/*/outbox_*.sql`
- **Charter**:
  1. **Lease Token Ownership**: Verify whether `mark_published()` and `mark_failed()` take and validate the `lease_token`. A superseded worker whose lease expired must not be able to mark a message published/failed after another relay reclaimed it.
  2. **Expiration Recovery SQL**: Verify `outbox_recover.sql`. Audit the condition `(lease_expires_at <= $1 OR ($1 - created_at) >= $2)`. Does reclaiming based on message age (`created_at`) violate active leases and cause double delivery?
  3. **Relay Error Propagation**: Audit `poll_and_dispatch()`. Verify whether errors from `recover_expired_claims()` are swallowed or properly propagated.
  4. **Duplicate Message Insertion**: Audit `INSERT INTO rume_outbox ... ON CONFLICT (id) DO UPDATE SET ...`. Does retrying an insert with an existing message ID overwrite status, resetting an already-published message to `pending` or altering its payload?
  5. **Transactional Atomicity**: Check `TransactionalOutbox::save_tx()` across Postgres, MySQL, and SQLite. Invariants: commit commits both; rollback rolls back both.

#### Lens 2: Persistence & Connection Pool Lifecycle
- **Output File**: `docs/reports/reviews/v03/pool-lifecycle-persistence.md`
- **Lead Subject**: `crates/core/src/persistence/pool.rs`, `crates/core/src/persistence/backend.rs`, `crates/core/src/persistence/config.rs`, `crates/core/src/persistence/{postgres,mysql,sqlite}.rs`
- **Charter**:
  1. **Pool Abstraction Delegation**: Audit `DatabasePool`. Does it maintain a disconnected semaphore and fictitious connection counters (`total_connections`), or does it honestly reflect and delegate to the underlying SQLx pool (`PgPool`, `MySqlPool`, `SqlitePool`)?
  2. **Physical Pool Lifecycle**: Does `pool.close()` actually close the underlying database pool (`PgPool::close`), and does `close_and_drain()` prove that physical connections and in-flight transactions are terminated?
  3. **Pool Configuration Forwarding**: Verify whether `DatabaseConfig` properties (`min_connections`, `idle_timeout_secs`, `connect_timeout_secs`) are forwarded to SQLx `PgPoolOptions` / `MySqlPoolOptions` / `SqlitePoolOptions`.
  4. **Ping & Health Checks**: Audit `ping()` and `health_check()` across backends. Are ping queries robust and do they detect dropped TCP connections?

#### Lens 3: Jobs & Worker Concurrency
- **Output File**: `docs/reports/reviews/v03/jobs-worker-concurrency.md`
- **Lead Subject**: `crates/core/src/jobs/`, `crates/core/src/jobs/postgres.rs`, `crates/core/src/jobs/redis.rs`, `crates/core/src/jobs/worker.rs`, `crates/core/src/jobs/reliability.rs`
- **Charter**:
  1. **Job Claim Lease Ownership**: Does `ack()`, `nack()`, and `dead_letter()` enforce lease token / worker ownership? Can a worker that lost its lease alter job status?
  2. **Active Job Lease Renewal / Heartbeat**: Can a long-running job (>60s) renew its lease while executing? Is there an API (`renew_lease` / `heartbeat`) on `JobQueue` and in `JobWorker`?
  3. **Worker Panic & Drain**: Verify worker drop guards, panic safety, and `worker.drain()` behavior during graceful shutdown.
  4. **DLQ & Idempotency Key Semantics**: Does a job that fails permanently land in DLQ with error context preserved? Does idempotency deduplication prevent concurrent duplicates without blocking retries of failed jobs?

---

### Stage 2: Security, Protocols & Ecosystem Verification

#### Lens 4: Distributed Coordination & Multi-Tenancy
- **Output File**: `docs/reports/reviews/v03/coordination-multi-tenant.md`
- **Lead Subject**: `crates/core/src/coordination/`, `crates/core/src/tenant/`, `crates/core/src/resilience/rate_limiter.rs`, `crates/core/src/cache/redis.rs`
- **Charter**:
  1. **Distributed Lock Fencing**: In `PostgresDistributedLock` and `RedisDistributedLock`, are fencing tokens monotonically increasing and strictly validated? Does lock renewal fail if the lease expired?
  2. **Leader Election Leases & Failover**: In `PostgresLeaderElector` and `RedisLeaderElector`, verify term monotonic increments and candidate takeover semantics upon lease expiration.
  3. **Multi-Tenant Boundary Isolation**: Audit `TenantContext`, `TenantScopedCache`, and `HeaderTenantResolver`/`SubdomainTenantResolver`. Are cross-tenant data leaks impossible across cache, rate limiting, and database connections?
  4. **Redis Coordination Scripts**: Audit Lua scripts in `crates/core/src/coordination/redis.rs` and `crates/core/src/jobs/redis.rs` for atomic key updates and script timeout safety.

#### Lens 5: Security, SAML & Protocol Hardening
- **Output File**: `docs/reports/reviews/v03/security-protocols.md`
- **Lead Subject**: `crates/core/src/enterprise/saml.rs`, `crates/core/src/security/`, `crates/core/src/auth/`
- **Charter**:
  1. **SAML XML Parsing & XSW Resistance**: Does `SamlParser` resist XML Signature Wrapping (XSW) attacks? Is the signed XML element cryptographically bound to the assertion extracted and consumed?
  2. **SAML Verification Enforcement**: Does `SamlParser::parse()` allow unverified assertions to be consumed in production, or should `parse_verified()` be the mandatory path?
  3. **Security Headers & CSRF**: Verify CSRF token signing, cookie tossing protection, SameSite enforcement, and constant-time token comparison.
  4. **Audit Logging & Secret Redaction**: Ensure audit log writes are durable, unforgeable, and do not leak credentials, passwords, or session secrets into plain text.

#### Lens 6: Testing & Feature Matrix Verification
- **Output File**: `docs/reports/reviews/v03/testing-feature-matrix.md`
- **Lead Subject**: `crates/core/Cargo.toml`, `Cargo.toml`, `crates/core/tests/` (all 24 suites)
- **Charter**:
  1. **Feature Gate Consistency**: Audit `enterprise_integration_pipeline.rs` and other test files. Are all feature dependencies properly gated (e.g. `#![cfg(feature = "postgres")]`) so `cargo check --no-default-features` or single-feature configurations compile cleanly?
  2. **Feature Matrix Testing**: Check buildability and test execution under matrix combinations:
     - `--no-default-features --features sqlite`
     - `--no-default-features --features postgres`
     - `--no-default-features --features mysql`
     - `--all-features`
  3. **Test Invariants & Flakiness**: Audit live container test timeouts, port allocations, cleanup on failure, and race conditions in concurrent tests.
  4. **Negative & Error Invariant Coverage**: Verify that tests assert real failure conditions (e.g., connection drop, lease loss, duplicate ID rejection) rather than only happy paths.
  5. **Test Directory Structure & Binary Explosion**: Audit `crates/core/tests/` (24 flat `.rs` files). Measure compilation overhead of compiling 24 distinct binaries and specify reorganization into consolidated test entrypoints (`tests/integration.rs`, `tests/containers.rs`, `tests/common/`).
  6. **Testcontainers Lifecycle & Reuse**: Audit container spin-up overhead across test suites. Specify reusable container fixtures with database/schema isolation (`UUID` DB names) to eliminate multi-second container restarts per test function.
  7. **Declarative Test Harness**: Audit setup boilerplate across test suites. Specify a first-party `RumeTestHarness` in `rume::testing` providing fluent test orchestration.
  8. **Testing Tooling Integration**: Evaluate adoption of `rstest` (fixtures/parameters), `insta` (snapshot testing for OpenAPI & RFC 9457 Problem Details), and `wiremock` (external HTTP IdP/webhook mocks).

---

## 5. Review Deliverables & Execution Rules

1. Each reviewer subagent produces exactly one report in `docs/reports/reviews/v03/<lens>.md`.
2. Reviewers follow `.opencode/skills/review-authoring/SKILL.md` (Summary, Prior Decisions, Findings with Where/What/Why/Fix, Positive Notes, Verdict).
3. Findings are strictly graded as Critical (`C`), Major (`M`), or Minor (`m`).
4. Conductor executes Stage 1 lenses in parallel, passes their reports to Stage 2 lenses, and collects all 6 lens reports.
5. Conductor derives `docs/reports/reviews/v03/todo.md` ordering all findings by execution priority (`P0`..`P3`) and grouping shared root causes.
