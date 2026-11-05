# Testing & Feature Matrix Diff Review — v03 (v01)

## Summary

This diff review evaluates the test architecture, cargo feature matrix, and container infrastructure in Rume across commits `cf82559..HEAD`, auditing `crates/core/Cargo.toml`, root `Cargo.toml`, and the consolidated test suites under `crates/core/tests/` (`integration.rs`, `containers.rs`, `common/`, `integration/`, `containers/`). Substantial test architecture modernization has landed: the 32 flat integration test binaries were successfully consolidated into 2 canonical test entrypoints (`integration` and `containers`), eliminating the 11 GB multi-binary link explosion; dev-dependencies adopted modern ecosystem crates (`insta`, `rstest`, `wiremock`, `ureq`); a declarative test harness (`RumeTestHarness`) was introduced; false-positive worker panic assertions were corrected with durable queue checks; and shared `OnceCell` container fixtures were implemented for live databases. However, the change **fails** diff review and cannot be accepted into production because single-feature matrix compilation remains broken for `mysql` and `redis` due to missing testcontainer imports in `common/containers.rs` (Finding C1), the supposedly "zero-docker" integration suite spawns 15 independent live Redis containers bypassing shared fixtures (Finding M1), the live container test suite exhibits 4 deterministic test failures (Finding M2), and 16 non-deterministic wall-clock sleeps remain across test suites (Finding M4).

---

## Prior decisions

All 11 prior testing findings recorded in `docs/reports/reviews/v03/testing-feature-matrix.md` have been audited against the diff between `cf82559` and `HEAD`:

- **v03 Testing C1 (Broken feature matrix: ungated backend dependencies break `cargo test` and `cargo check` under single-feature and default configurations)** — **CHALLENGED (Defective Remediation)**.
  The consolidation of 32 flat binaries into `tests/integration.rs` and `tests/containers.rs` added module-level `#[cfg(feature = "...")]` attributes, successfully unblocking default builds (`cargo check -p rume --tests` with default `sqlite`), `cargo check -p rume --tests --no-default-features`, and single-feature `postgres` / `sqlite` configurations. However, the remediation is defective: in `crates/core/tests/common/containers.rs:16, 18`, `ContainerAsync`, `ImageExt`, and `AsyncRunner` were conditionally imported *only* under `#[cfg(feature = "postgres")]`. When compiling single-feature builds without `postgres`, `cargo check -p rume --tests --no-default-features --features mysql` fails with `E0425: cannot find type ContainerAsync` and `E0599: no method named start found for struct Mysql`, and `cargo check -p rume --tests --no-default-features --features redis` fails with `E0425: cannot find type ContainerAsync` and `E0599: no method named with_tag found for struct Redis`. Single-feature build matrix isolation remains broken. Re-reported as Finding C1.

- **v03 Testing C2 (False-positive assertion and panic masking in `jobs_suite.rs` leaves corrupted durable database state undetected)** — **RESOLVED**.
  In `crates/core/tests/containers/jobs.rs:326–342` (formerly `jobs_suite.rs:293–310`), the test was refactored with explicit deterministic synchronization (`started: Arc<tokio::sync::Notify>`) and updated to assert durable database invariants:
  ```rust
  let drained = worker.drain(Duration::from_millis(200)).await;
  assert!(drained);

  // Durable queue invariant: panicked job must not be orphaned in 'in_flight'
  assert_eq!(queue.dead_letter_count().await.unwrap(), 1, "panicked job must be routed to DLQ");
  assert_eq!(queue.pending_count().await.unwrap(), 0);
  ```
  The false-positive assertion has been completely eliminated. Crucially, as verified by `jobs-worker-concurrency.md` (Finding M1), this test now correctly fails against `HEAD` because `JobWorker`'s panic handler wraps only future instantiation rather than execution, proving that the test now reliably detects corrupted database state.

- **v03 Testing C3 (Potemkin pool lifecycle test in `postgres_container_suite.rs` asserts boolean flags instead of physical connection drain)** — **REAFFIRMED (Unresolved in PostgreSQL container suite)**.
  While `crates/core/tests/integration/persistence.rs:357–360` updated the SQLite pool lifecycle test to assert `pool.backend().is_closed()` and verify `ping()` returns `Err(PersistenceError::Closed)`, `test_postgres_pool_lifecycle_and_health` in `crates/core/tests/containers/postgres.rs:77–81` was left unmodified:
  ```rust
  // Verify graceful closure
  let closed = pool.close_and_drain(Duration::from_millis(500)).await;
  assert!(closed);
  assert!(pool.status().is_closed);
  ```
  It continues to verify only the boolean return value and atomic flag. It never checks that post-closure queries return `Err(PersistenceError::PoolClosed)` or that the physical PostgreSQL driver connection pool is actually terminated (`pool.backend().is_closed()`). Re-reported as Finding M3.

- **v03 Testing M1 (Test directory binary explosion: 32 flat test crates cause 11 GB build bloat and extreme link overhead)** — **RESOLVED**.
  The 32 flat `.rs` files in `crates/core/tests/` were reorganized into 2 canonical test entrypoints defined in `crates/core/Cargo.toml:61–67`:
  - `[[test]] name = "integration"`, `path = "tests/integration.rs"`
  - `[[test]] name = "containers"`, `path = "tests/containers.rs"`
  - Shared fixtures consolidated into `crates/core/tests/common/` (`mod.rs`, `containers.rs`).
  Cargo link steps dropped by 94% (from 32 down to 2), saving ~9 GB of disk space in `target/debug/deps` and enabling fast local execution: `cargo test --test integration` executes 127 tests in 90 milliseconds.

- **v03 Testing M2 (Per-test container spin-up overhead adds 3+ minutes of idle latency and risks Docker daemon exhaustion)** — **CHALLENGED (Partially Resolved)**.
  `crates/core/tests/common/containers.rs` introduced `tokio::sync::OnceCell` shared container fixtures with per-test UUID database isolation (`shared_postgres()`, `shared_mysql()`, `shared_redis()`). All test files under `crates/core/tests/containers/` (`postgres.rs`, `mysql.rs`, `redis.rs`, `jobs.rs`, `job_reliability.rs`, `pipeline.rs`) successfully migrated to these shared fixtures. However, the remediation was incomplete: four test suites under `crates/core/tests/integration/` (`auth.rs`, `cache.rs`, `coordination.rs`, `tenant.rs`) were left with private helper functions that spin up 15 independent Redis containers directly with `Redis::default().start().await`. Re-reported as Finding M1.

- **v03 Testing M3 (Persistent async wall-clock sleeps across 14 test suites violate determinism and introduce flaky races)** — **REAFFIRMED (Incomplete Remediation)**.
  The total number of raw `sleep` calls was reduced from 34 down to 16 through the adoption of `tokio::sync::Notify` in `jobs.rs` and `pipeline.rs`, and `tokio::time::pause()` in `resilience.rs`. However, 16 raw `tokio::time::sleep()` calls remain across 6 test suites (`job_reliability.rs`, `jobs.rs`, `mysql.rs`, `postgres.rs`, `redis.rs`, `cache.rs`, `coordination.rs`, `lifecycle.rs`), including sleeps up to 2 seconds (`mysql.rs:254`) and 1.1 seconds (`redis.rs:580`). Re-reported as Finding M4.

- **v03 Testing M4 (Absence of a first-party declarative test harness (`RumeTestHarness`) forces repetitive boilerplate across test suites)** — **RESOLVED**.
  `RumeTestHarness` was introduced in `crates/core/src/testing/harness.rs:8–100` with fluent builders:
  - `RumeTestHarness::in_memory()` (SQLite) and `RumeTestHarness::postgres(pool)`
  - `.with_router(router)`, `.with_outbox(dispatcher)`
  - Direct execution helpers: `.execute(stmt)`, `.query_scalar(query)`, `.get(uri)`, `.post_json(uri, body)`
  - Fully tested by unit tests in `src/testing/harness.rs:108–126`.

- **v03 Testing M5 (Lack of modern testing ecosystem tooling (`insta`, `rstest`, `wiremock`) hinders snapshot and protocol verification)** — **RESOLVED**.
  `crates/core/Cargo.toml:56–59` added `insta = { version = "1.49", features = ["json"] }`, `rstest = "0.28"`, `wiremock = "0.6"`, and `ureq = "3.5"`.
  - `insta::assert_json_snapshot!` was adopted in `integration/openapi.rs:32, 60` and `integration/error.rs:62, 99` for OpenAPI and Problem Details JSON snapshot regression testing.
  - `rstest::rstest` was adopted in `integration/routing.rs:135–158` for parameterized HTTP route test cases.
  - `wiremock::MockServer` and `ureq` were adopted in `integration/enterprise.rs:145–170` for live HTTP IdP metadata discovery simulation.

- **v03 Testing m1 (Inconsistent test file naming conventions across `crates/core/tests/`)** — **RESOLVED**.
  Standardized noun modules were established under `crates/core/tests/integration/` (`auth.rs`, `cache.rs`, `composition.rs`, `config.rs`, `coordination.rs`, `enterprise.rs`, `error.rs`, `events.rs`, `health.rs`, `lifecycle.rs`, `metrics.rs`, `openapi.rs`, `outbox.rs`, `persistence.rs`, `routing.rs`, `security.rs`, `tenant.rs`, `validation.rs`) and `crates/core/tests/containers/` (`postgres.rs`, `mysql.rs`, `redis.rs`, `jobs.rs`, `job_reliability.rs`, `pipeline.rs`).

- **v03 Testing m2 (Redundant test coverage between unit tests and integration tests)** — **REAFFIRMED (Unresolved)**.
  `health_routes.rs` was renamed to `integration/health.rs` with 100% identical test logic, duplicating the internal health state transition tests already present in `crates/core/src/routing/health.rs`. `integration/error.rs` retains redundant manual field assertions alongside the newly added `insta` snapshots. Re-reported as Finding m1.

- **v03 Testing m3 (Duplicate container spin-up logic across PostgreSQL and Redis suites)** — **RESOLVED (In containers suite)**.
  Centralized container lifecycle and connection URL configuration was implemented in `crates/core/tests/common/containers.rs` and reused across all container modules. (Integration module Redis duplication is tracked in Finding M1).

---

## Findings

### C1 Single-feature cargo check and test builds fail for `mysql` and `redis` due to missing testcontainer imports in `common/containers.rs`

- **Where:** `crates/core/tests/common/containers.rs:11–20, 81–93, 138–151`
- **What it says:**
  In `crates/core/tests/common/containers.rs`:
  ```rust
  // ─── PostgreSQL Shared Container & Isolation ─────────────────────────────────

  #[cfg(feature = "postgres")]
  use testcontainers_modules::postgres::Postgres;
  #[cfg(feature = "postgres")]
  use testcontainers_modules::testcontainers::ImageExt;
  #[cfg(feature = "postgres")]
  use testcontainers_modules::testcontainers::runners::AsyncRunner;
  #[cfg(feature = "postgres")]
  use testcontainers_modules::testcontainers::ContainerAsync;
  ...
  // ─── MySQL Shared Container & Isolation ──────────────────────────────────────

  #[cfg(feature = "mysql")]
  use testcontainers_modules::mysql::Mysql;

  #[cfg(feature = "mysql")]
  static MYSQL_NODE: OnceCell<ContainerAsync<Mysql>> = OnceCell::const_new();

  #[cfg(feature = "mysql")]
  pub async fn shared_mysql() -> &'static ContainerAsync<Mysql> {
      MYSQL_NODE
          .get_or_init(|| async {
              Mysql::default()
                  .start()
                  .await
                  .expect("start shared mysql container")
          })
          .await
  }
  ...
  // ─── Redis Shared Container & Isolation ──────────────────────────────────────

  #[cfg(feature = "redis")]
  use testcontainers_modules::redis::{REDIS_PORT, Redis};

  #[cfg(feature = "redis")]
  static REDIS_NODE: OnceCell<ContainerAsync<Redis>> = OnceCell::const_new();

  #[cfg(feature = "redis")]
  pub async fn shared_redis() -> &'static ContainerAsync<Redis> {
      REDIS_NODE
          .get_or_init(|| async {
              Redis::default()
                  .with_tag("alpine")
                  .start()
                  .await
                  .expect("start shared redis container")
          })
          .await
  }
  ```
- **Why it's a problem:**
  In `common/containers.rs`, the generic testcontainer traits and structures (`ContainerAsync`, `AsyncRunner`, and `ImageExt`) are conditionally imported *only* under `#[cfg(feature = "postgres")]` (lines 14–18).
  When building or checking the test targets under documented single-feature configurations without `postgres`:
  1. `cargo check -p rume --tests --no-default-features --features mysql` fails compilation:
     ```
     error[E0425]: cannot find type `ContainerAsync` in this scope
       --> crates/core/tests/common/containers.rs:81:29
        |
     81 | static MYSQL_NODE: OnceCell<ContainerAsync<Mysql>> = OnceCell::const_new();
        |                             ^^^^^^^^^^^^^^ not found in this scope

     error[E0599]: no method named `start` found for struct `Mysql` in the current scope
       --> crates/core/tests/common/containers.rs:88:18
        |
     88 |                 .start()
        |                  ^^^^^ method not found in `Mysql`
     ```
  2. `cargo check -p rume --tests --no-default-features --features redis` fails compilation:
     ```
     error[E0425]: cannot find type `ContainerAsync` in this scope
        --> crates/core/tests/common/containers.rs:138:29
         |
     138 | static REDIS_NODE: OnceCell<ContainerAsync<Redis>> = OnceCell::const_new();
         |                             ^^^^^^^^^^^^^^ not found in this scope

     error[E0599]: no method named `with_tag` found for struct `testcontainers_modules::redis::Redis` in the current scope
        --> crates/core/tests/common/containers.rs:145:18
         |
     145 |                 .with_tag("alpine")
         |                  ^^^^^^^^ method not found in `testcontainers_modules::redis::Redis`
     ```
  This violates feature isolation: downstream consumers selecting only `mysql` or `redis` cannot compile their test targets, and single-feature matrix CI jobs fail immediately.
- **Suggested fix:**
  In `crates/core/tests/common/containers.rs`, move the testcontainer utility imports out of the `postgres`-only block and gate them under all container features:
  ```rust
  #[cfg(any(feature = "postgres", feature = "mysql", feature = "redis"))]
  use testcontainers_modules::testcontainers::{
      ContainerAsync, ImageExt, runners::AsyncRunner,
  };
  ```

---

### M1 Contamination of "zero-docker" integration test suite with unshared live Redis container spin-ups

- **Where:** `crates/core/tests/integration.rs:1–18`, `crates/core/tests/integration/auth.rs:12–28`, `crates/core/tests/integration/cache.rs:21–35`, `crates/core/tests/integration/coordination.rs:9–26`, `crates/core/tests/integration/tenant.rs:254–270`
- **What it says:**
  `crates/core/tests/integration.rs:1` declares:
  ```rust
  //! Fast zero-docker integration suites.
  ```
  However, four test modules included directly in `integration.rs` independently launch live Docker containers:
  - `integration/auth.rs`: `start_redis_session_store()` calls `Redis::default().start().await` across 3 test cases (lines 149, 235, 364).
  - `integration/cache.rs`: `start_redis_cache()` calls `Redis::default().start().await` across 6 test cases (lines 38, 74, 126, 162, 183, 205).
  - `integration/coordination.rs`: `start_redis_coordination()` calls `Redis::default().start().await` across 3 test cases (lines 31, 64, 93).
  - `integration/tenant.rs`: `start_redis_tenant_cache()` calls `Redis::default().start().await` across 3 test cases (lines 277, 331, 393).
- **Why it's a problem:**
  1. **Architectural boundary violation**: The test reorganization chartered two cleanly separated binaries: `integration` (fast in-memory / zero-docker) and `containers` (live docker infrastructure). Running `cargo test --test integration --all-features` unexpectedly launches **15 independent Docker containers**, requiring an active Docker daemon and adding over 60 seconds of latency to what was advertised as a fast zero-docker suite.
  2. **Bypassing shared fixtures**: These 15 tests bypass the `OnceCell` shared container fixture in `common::containers::shared_redis()`. They redundantly spin up and tear down a new Redis container for every test function, re-introducing Docker daemon socket saturation, bridge IP exhaustion, and container launch overhead.
- **Suggested fix:**
  1. Move container-dependent tests (`auth.rs`, `cache.rs`, `coordination.rs`, `tenant.rs`) into `tests/containers/` (or merge into `tests/containers/redis.rs`).
  2. For tests remaining in `tests/integration/`, utilize in-memory mock session and cache stores for true zero-docker execution, or migrate any unavoidable live Redis tests to use `common::containers::shared_redis()`.

---

### M2 Container test suite regressions: four deterministic test failures across PostgreSQL, jobs, and Redis

- **Where:**
  - `crates/core/tests/containers/postgres.rs:119` (`test_postgres_migrations_up_and_rollback`)
  - `crates/core/tests/containers/jobs.rs:340` (`test_worker_panic_does_not_leak_in_flight_count`)
  - `crates/core/tests/containers/job_reliability.rs:256` (`test_worker_heartbeat_and_abandoned_job_recovery`)
  - `crates/core/tests/containers/redis.rs:534` (`test_redis_idempotency_store`)
- **What it says:**
  Running `cargo test --test containers --all-features` results in 4 failed tests:
  1. `postgres::test_postgres_migrations_up_and_rollback` panics at line 119:
     ```
     migrations should succeed: QueryFailed("error returned from database: column \"name\" of relation \"rume_migrations\" does not exist")
     ```
  2. `jobs::worker_panic_does_not_leak_in_flight_count` panics at line 340:
     ```
     assertion `left == right` failed: panicked job must be routed to DLQ
       left: 0
      right: 1
     ```
  3. `job_reliability::test_worker_heartbeat_and_abandoned_job_recovery` panics at line 256:
     ```
     assertion failed: claimed.is_some()
     ```
  4. `redis::test_redis_idempotency_store` panics at line 534:
     ```
     assertion `left == right` failed
       left: Completed(Some([111, 114, 100, 101, 114, 45, 105, 100, 58, 52, 53, 54]))
      right: New
     ```
- **Why it's a problem:**
  The live container integration suite is broken on `HEAD`. These failures reflect severe defects in underlying subsystems:
  - In `postgres.rs`: Preflight checks in `Migrator::verify_no_pending_backend` execute obsolete 2-column DDL, creating a truncated table that causes subsequent `run_pending_backend` migrations to abort with missing column errors.
  - In `jobs.rs`: `JobWorker::execute_job` wraps future construction in `catch_unwind` rather than execution; asynchronous handler panics unwind uncaught, leaving jobs permanently stranded in `status = 'in_flight'` with zero DLQ recording.
  - In `job_reliability.rs`: `worker1.start()` is spawned before the test enqueues and dequeues the crashed job; `worker1` steals the job immediately, causing `dequeue_for("crashed-worker-99")` to return `None`.
  - In `redis.rs`: `RedisIdempotencyStore::release` was restricted to `IN_PROGRESS` keys, making it a no-op against `COMPLETED:` records and preventing key eviction or replay.
- **Suggested fix:**
  Land the subsystem fixes identified across the respective reviews:
  1. In `crates/core/src/persistence/migration.rs:100, 253`: update the DDL to create `rume_migrations` with all 4 columns (`name` and `checksum` with `DEFAULT ''`).
  2. In `crates/core/src/jobs/worker.rs:366–371`: use `FutureExt::catch_unwind` from `futures-util` to intercept async handler panics and route them to `queue.dead_letter()`.
  3. In `crates/core/tests/containers/job_reliability.rs:234–256`: enqueue and claim the job under `crashed-worker-99` *before* calling `worker1.start()`.
  4. In `crates/core/src/jobs/redis.rs:320–334`: update the Lua script to delete both `IN_PROGRESS` and `COMPLETED:` records.

---

### M3 Potemkin pool lifecycle assertions in `containers/postgres.rs` persist without verifying query rejection or connection termination

- **Where:** `crates/core/tests/containers/postgres.rs:77–81`
- **What it says:**
  ```rust
  // Verify graceful closure
  let closed = pool.close_and_drain(Duration::from_millis(500)).await;
  assert!(closed);
  assert!(pool.status().is_closed);
  ```
- **Why it's a problem:**
  While the SQLite suite in `crates/core/tests/integration/persistence.rs:357–360` was updated to assert `pool.backend().is_closed()` and verify that post-closure queries return `Err(PersistenceError::Closed)`, the PostgreSQL container suite was left untouched.
  The test asserts only that the method returns `true` and that the atomic flag `pool.status().is_closed` is `true`. It never verifies:
  1. Whether executing a query against the PostgreSQL pool after closure returns `Err(PersistenceError::PoolClosed)`.
  2. Whether the physical SQLx driver pool was actually closed (`pool.backend().is_closed()`).
  Asserting internal boolean flags rather than physical database invariants leaves the PostgreSQL pool lifecycle verification incomplete.
- **Suggested fix:**
  Update `test_postgres_pool_lifecycle_and_health` in `crates/core/tests/containers/postgres.rs` to verify physical pool closure and post-close query rejection:
  ```rust
  let closed = pool.close_and_drain(Duration::from_millis(500)).await;
  assert!(closed);
  assert!(pool.status().is_closed);
  assert!(pool.backend().is_closed());

  let ping_res = pool.backend().ping().await;
  assert!(matches!(ping_res, Err(PersistenceError::Closed)));
  ```

---

### M4 Persistent non-deterministic wall-clock sleeps across 6 integration and container test suites

- **Where:** 16 occurrences across 6 test suites:
  - `crates/core/tests/containers/job_reliability.rs:113, 238, 260`
  - `crates/core/tests/containers/jobs.rs:258, 390`
  - `crates/core/tests/containers/mysql.rs:254`
  - `crates/core/tests/containers/postgres.rs:554`
  - `crates/core/tests/containers/redis.rs:109, 340, 478, 580`
  - `crates/core/tests/integration/cache.rs:175, 217`
  - `crates/core/tests/integration/coordination.rs:76`
  - `crates/core/tests/integration/lifecycle.rs:182, 190`
- **What it says:**
  Suites still rely on arbitrary wall-clock sleeps up to 2,000ms (`mysql.rs:254`: `tokio::time::sleep(Duration::from_secs(2)).await;`) and 1,100ms (`redis.rs:580`: `tokio::time::sleep(Duration::from_millis(1100)).await;`) to wait for lease expiration, rate limit sliding windows, or background state transitions:
  ```rust
  // job_reliability.rs:238
  sleep(Duration::from_millis(150)).await;
  assert!(heartbeats.is_alive("worker-active-1").await.unwrap());
  ```
- **Why it's a problem:**
  1. **CI Flakiness**: A 150ms sleep assumes the OS thread scheduler will dispatch the background heartbeat task within that window. Under CPU contention on shared CI runners, 150ms is frequently insufficient, causing false-negative test failures.
  2. **Cumulative Latency**: The 16 remaining sleeps add over 6 seconds of dead idle waiting to every complete test run.
  3. **Incomplete Virtual Time Adoption**: While `crates/core/tests/integration/resilience.rs:211, 245` correctly demonstrated `tokio::time::pause()` and `tokio::time::advance()` for instantaneous, 100% deterministic time simulation, time-dependent tests in `cache.rs`, `coordination.rs`, and `lifecycle.rs` failed to adopt it.
- **Suggested fix:**
  1. Replace state-synchronization sleeps with `tokio::sync::Notify`, `watch` channels, or atomic flags.
  2. Wrap in-memory TTL and sliding window tests with `tokio::time::pause()` and `tokio::time::advance()`.
  3. For live container lease expiration tests, reduce test lease TTLs (e.g. to 50–100ms) and poll with short 5ms retries capped by a `tokio::time::timeout`.

---

### m1 Redundant test assertions between unit test suites and integration test modules

- **Where:** `crates/core/src/routing/health.rs:362–660` vs `crates/core/tests/integration/health.rs:20–120`; `crates/core/src/error.rs:286–455` vs `crates/core/tests/integration/error.rs:59–120`
- **What it says:**
  `crates/core/tests/integration/health.rs` is an exact verbatim copy of the legacy `health_routes.rs`, testing health check state transitions and degraded states already covered in unit tests in `crates/core/src/routing/health.rs`. In `crates/core/tests/integration/error.rs`, lines 71–76 and 108–113 retain redundant manual field-by-field assertions (`assert_eq!(body["type"].as_str(), ...)`) directly after `insta::assert_json_snapshot!(body)`.
- **Why it's a problem:**
  Exact duplicate assertions inflate compile and execution times without improving defect detection coverage.
- **Suggested fix:**
  Prune manual JSON field assertions that duplicate `insta` snapshots in `integration/error.rs`, and restrict `integration/health.rs` to Axum HTTP router endpoint verification (`/health/live`, `/health/ready`).

---

### m2 Unused imports in `crates/core/src/testing/harness.rs` generate compiler warnings

- **Where:** `crates/core/src/testing/harness.rs:3, 4, 6, 103, 104, 105`
- **What it says:**
  Compiling the crate emits 6 compiler warnings:
  - `warning: unused import: axum::body::Body` (line 3)
  - `warning: unused import: axum::http::Response` (line 4)
  - `warning: unused import: super::app::TestApp` (line 6)
  - `warning: unused import: super::*` (line 103)
  - `warning: unused import: axum::http::StatusCode` (line 104)
  - `warning: unused import: axum::routing::get` (line 105)
- **Why it's a problem:**
  Compiler warnings add diagnostic clutter to `cargo check` and CI test output, violating clean-build conventions.
- **Suggested fix:**
  Remove the unused imports or gate them under `#[cfg(any(feature = "sqlite", feature = "postgres", feature = "mysql"))]`.

---

## Positive notes

- **Binary Consolidation Slashing Build Bloat**: Reorganizing 32 flat integration test binaries into 2 canonical entrypoints (`tests/integration.rs` and `tests/containers.rs`) eliminated the 11 GB multi-binary link overhead, reducing disk usage by ~80% and dropping local integration test execution time to 90ms.
- **Elimination of False-Positive Worker Panic Assertion**: Refactoring `worker_panic_does_not_leak_in_flight_count` in `tests/containers/jobs.rs` to assert `queue.dead_letter_count() == 1` and `queue.pending_count() == 0` eliminated the false-positive assertion that masked durable database corruption in v03.
- **First-Party Declarative Test Harness (`RumeTestHarness`)**: The introduction of `RumeTestHarness` in `crates/core/src/testing/harness.rs` provides a fluent, ergonomic API for assembling test applications, database pools, and outbox relays with minimal boilerplate.
- **Modern Testing Ecosystem Adoption**: Integrating `insta` (snapshot testing), `rstest` (parameterized test cases), `wiremock` (HTTP mock server), and `ureq` significantly modernizes API schema validation and external IdP protocol simulation.
- **Shared Container Fixtures with Database Isolation**: `crates/core/tests/common/containers.rs` successfully implemented `OnceCell` container singletons with dynamic `CREATE DATABASE test_<uuid>` isolation, reducing container spin-up overhead from 46 containers down to 3 for the containers suite.
- **Strict `#![forbid(unsafe_code)]` Enforcement**: Production code in `crates/core/src/lib.rs:8`, container tests in `crates/core/tests/containers.rs:3`, and shared fixtures in `crates/core/tests/common/containers.rs:3` strictly enforce `#![forbid(unsafe_code)]`.
- **Rapid Sub-Second Zero-Docker Verification**: Running `cargo test --test integration --features sqlite` executes 127 integration tests across routing, validation, error handling, security headers, metrics, and saga orchestration in under 0.1 seconds.

---

## Verdict

**Fail** (Must land fix pass first).

The test architecture and cargo feature matrix have achieved significant structural improvements across binary consolidation, shared container fixtures, and ecosystem tooling. However, the change cannot be accepted into production until a fix pass resolves the following blockers:

1. **C1** — Fix single-feature test compilation in `crates/core/tests/common/containers.rs` by gating `ContainerAsync`, `ImageExt`, and `AsyncRunner` under `#[cfg(any(feature = "postgres", feature = "mysql", feature = "redis"))]`, ensuring `cargo check -p rume --tests --no-default-features --features mysql` and `--features redis` compile cleanly.
2. **M1** — Stop launching 15 unshared live Redis containers inside the supposedly zero-docker `tests/integration.rs` suite; move live Redis tests to `tests/containers/` and use `common::containers::shared_redis()`.
3. **M2** — Fix the 4 failing container tests (`test_postgres_migrations_up_and_rollback`, `test_worker_panic_does_not_leak_in_flight_count`, `test_worker_heartbeat_and_abandoned_job_recovery`, and `test_redis_idempotency_store`).
4. **M3** — Update `test_postgres_pool_lifecycle_and_health` in `crates/core/tests/containers/postgres.rs` to verify physical pool closure (`pool.backend().is_closed()`) and query rejection.
5. **M4** — Replace the 16 remaining non-deterministic wall-clock sleeps with `tokio::sync::Notify` and paused virtual time (`tokio::time::pause()`).
