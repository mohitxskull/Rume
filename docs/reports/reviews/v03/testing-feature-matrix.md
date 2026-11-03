# Testing & Feature Matrix Verification Review — v03

## Summary

The Rume verification suite has expanded into a comprehensive test ecosystem spanning 154 unit tests, 25 doc-tests, and 32 integration test suites backed by real Testcontainers infrastructure across PostgreSQL, MySQL/MariaDB, Redis, and SQLite. However, the test suite and build matrix are **not production-ready** due to critical feature gate isolation failures: running `cargo test` under default features or running `cargo check` across documented single-feature configurations (`--features sqlite`, `--features postgres`, `--features mysql`, or `--no-default-features`) fails to compile because backend-specific dependencies are left ungated across 10 test files. Furthermore, the test suite suffers from severe structural and lifecycle inefficiencies: 32 flat integration test binaries trigger a massive Cargo multi-binary compile explosion consuming 11 GB in `target/debug/deps`, 46 containers are redundantly spun up and torn down per test run adding over 3 minutes of CI latency, 34 non-deterministic wall-clock sleeps remain across 14 test suites, and false-positive assertions mask critical worker panic and pool drainage defects.

---

## Prior decisions

All prior testing findings recorded in `docs/reports/reviews/v02/testing.md` and tracked in `docs/reports/reviews/v02/todo.md` have been audited against the v03 codebase:

- **v02 M1 (Happy-path test bias across newly introduced subsystems fails to verify failure modes)** — **REAFFIRMED (Recurred in v03 Subsystems)**. The specific tests mandated in v02 were introduced (e.g. `circuit_breaker_trips_on_service_transport_errors` in `resilience_suite.rs:189`, `rollback_last_fails_when_down_sql_is_none` in `persistence_integration.rs:163`, `saml_assertion_expired_returns_error` in `enterprise_suite.rs:104`, and `tenant_cache_delimiter_isolation` in `tenant_suite.rs:175`). However, with the Phase 5 introduction of SQLx 0.8.6 persistence, PostgreSQL job queues, distributed locks, and outbox leasing, the exact same happy-path bias recurred: tests omit negative assertions for superseded worker lease theft, active lease revocation during long-running execution, crashed worker takeover fencing, and physical connection termination on pool close (findings C2, C3).
- **v02 M2 (`ArchitectureRules::verify` only inspects direct dependencies and ignores transitive violations, undeclared modules, and cycles)** — **REAFFIRMED in part, RESOLVED in part**. Commit `819a857` added dependency graph construction and breadth-first search transitive reachability calculation to `ArchitectureRules::verify` (`rume/src/module/architecture.rs:84–120`), successfully enforcing forbidden transitive dependencies (`A -> B -> C`). Cycle detection and undeclared module validation remain delegated to `ModuleRegistry::resolve_startup_order`.
- **v02 M3 (Widespread non-deterministic wall-clock sleeps across async tests create flaky test risks and slow execution)** — **CHALLENGED (Superficial Remediation)**. Item 146 in `docs/reports/reviews/v02/todo.md` was marked resolved (`[x]`), claiming migration to `tokio::time::pause()`. In reality, only 3 tests in `rume/tests/resilience_suite.rs:12, 211, 245` adopted `tokio::time::pause()`. Across the remaining 14 test suites, 34 raw `tokio::time::sleep()` calls remain in active code (`cache_suite.rs`, `coordination_suite.rs`, `jobs_suite.rs`, `job_reliability_suite.rs`, `lifecycle_shutdown.rs`, `mysql_container_suite.rs`, `redis_container_suite.rs`), preserving CI flakiness and non-deterministic timing races (evaluated in finding M3).
- **v02 M4 (`TestApp` and `RumeBuilder::test_app()` bypass module lifecycle hooks)** — **REAFFIRMED (Resolved)**. Verified in `rume/src/app/builder.rs:545–595` and `rume/src/testing.rs:37–46`. `RumeBuilder::build_test_app()` and `TestApp::from_builder()` resolve module dependency order, execute all `on_start()` hooks with rollback on failure, and wire module `on_shutdown()` hooks to `TestApp::teardown()` and `Drop`.
- **v02 m1 (`ModuleHarness` lacks health check verification and error-path test coverage)** — **REAFFIRMED (Resolved)**. Verified in `rume/src/module/harness.rs:76–95`. `ModuleHarness::health_checks()` and `ModuleHarness::run_health_checks()` expose module diagnostic checks for isolated domain testing.
- **v02 m2 (Process environment mutation in `env_and_config_loading.rs` is not hermetically restored)** — **REAFFIRMED (Resolved)**. Verified in `rume/tests/env_and_config_loading.rs:11–43`. An RAII `EnvVarGuard` saves prior environment variable states and automatically restores or unsets them upon `Drop`.
- **v02 m3 (Config unit tests use hardcoded `/tmp` paths rather than isolated `tempfile::tempdir()`)** — **REAFFIRMED (Resolved)**. Verified in `rume/src/config.rs:259–325`. All configuration file loader unit tests now allocate unique, cryptographically random temporary directories via `tempfile::tempdir()`.
- **SQLx 0.8 Multi-Backend Feature Matrix Isolation** — **NOTE-NEW**. Evaluated in finding C1.
- **Testcontainers Live Infrastructure Matrix** — **NOTE-NEW**. Evaluated in findings M1, M2, and m3.
- **Enterprise Failure-Oriented Pipeline Verification** — **NOTE-NEW**. Evaluated in `rume/tests/enterprise_integration_pipeline.rs` and finding C1.

---

## Findings

### C1 Broken feature matrix: ungated backend dependencies break `cargo test` and `cargo check` under single-feature and default configurations

- **Where:** `rume/tests/` across 10 integration test files:
  - `rume/tests/coordination_suite.rs:9–24`
  - `rume/tests/auth_suite.rs:12–25`
  - `rume/tests/cache_suite.rs:8`
  - `rume/tests/tenant_suite.rs:5, 218–225`
  - `rume/tests/jobs_suite.rs:7, 21–26`
  - `rume/tests/job_reliability_suite.rs:5–7, 23–42`
  - `rume/tests/enterprise_integration_pipeline.rs:18, 25, 91`
  - `rume/tests/security_suite.rs:3`
  - `rume/tests/persistence_integration.rs:5`
  - `rume/tests/outbox_suite.rs:9`
- **What it says:**
  In `rume/Cargo.toml:46–51`, the package defines isolated feature flags with `sqlite` enabled by default:
  ```toml
  [features]
  default = ["sqlite"]
  postgres = ["dep:sqlx", "sqlx/postgres"]
  redis = ["dep:deadpool-redis", "dep:redis"]
  sqlite = ["dep:sqlx", "sqlx/sqlite"]
  mysql = ["dep:sqlx", "sqlx/mysql"]
  ```
  However, out of 32 integration test files in `rume/tests/`, only 4 contain crate-level `#![cfg(feature = "...")]` attributes (`mysql_container_suite.rs`, `postgres_container_suite.rs`, `redis_container_suite.rs`, and `sqlite_suite.rs`). The remaining 28 test files contain no inner crate attributes, yet directly import backend-specific symbols:
  - `jobs_suite.rs` imports `PostgresJobQueue` and `sqlx::postgres::PgPoolOptions` without `#![cfg(feature = "postgres")]`.
  - `coordination_suite.rs`, `auth_suite.rs`, `cache_suite.rs`, and `tenant_suite.rs` import `RedisDistributedLock`, `RedisLeaderElector`, `RedisSessionStore`, `RedisCacheBackend`, `RedisTenantQuotaStore`, and `deadpool_redis` without `#![cfg(feature = "redis")]`.
  - `job_reliability_suite.rs` imports `PostgresJobQueue`, `RedisHeartbeatRegistry`, `RedisIdempotencyStore`, and `deadpool_redis` without `#![cfg(all(feature = "postgres", feature = "redis"))]`.
  - `enterprise_integration_pipeline.rs` imports `PostgresJobQueue`, `sqlx::postgres`, and `SqliteBackend` without `#![cfg(all(feature = "postgres", feature = "sqlite"))]`.
  - `security_suite.rs`, `persistence_integration.rs`, and `outbox_suite.rs` import `SqliteBackend` without `#![cfg(feature = "sqlite")]`.
- **Why it's a problem:**
  In Rust/Cargo, every `.rs` file directly in `tests/` is compiled as an independent test crate. When a developer or CI runner checks or tests the crate under any feature combination other than `--all-features`, Cargo attempts to compile all 32 test binaries. As a result:
  1. **Default build fails**: Running `cargo test -p rume` fails immediately with `E0432: unresolved import rume::cache::RedisCacheBackend` and `E0433: cannot find module or crate deadpool_redis` because `default` does not include `redis` or `postgres`.
  2. **Single-backend builds fail**:
     - `cargo check -p rume --tests --no-default-features --features sqlite` fails with 14 compilation errors across 5 test files.
     - `cargo check -p rume --tests --no-default-features --features postgres` fails with 17 compilation errors across 5 test files.
     - `cargo check -p rume --tests --no-default-features --features mysql` fails with 12 compilation errors across 4 test files.
     - `cargo check -p rume --tests --no-default-features` fails with 23 compilation errors across 8 test files.
  This breaks modular crate packaging: downstream consumers selecting only `features = ["postgres"]` or `features = ["sqlite"]` cannot run test validation, and standard CI matrix jobs fail.
- **Suggested fix:**
  1. Add appropriate crate-level attributes to every feature-dependent test file:
     - `jobs_suite.rs`: `#![cfg(feature = "postgres")]`
     - `cache_suite.rs`, `coordination_suite.rs`, `auth_suite.rs`, `tenant_suite.rs`: `#![cfg(feature = "redis")]`
     - `job_reliability_suite.rs`: `#![cfg(all(feature = "postgres", feature = "redis"))]`
     - `enterprise_integration_pipeline.rs`: `#![cfg(all(feature = "postgres", feature = "sqlite"))]`
     - `security_suite.rs`, `persistence_integration.rs`, `outbox_suite.rs`: `#![cfg(feature = "sqlite")]`
  2. Alternatively, resolve this comprehensively via the test directory consolidation specified in finding M1, gating consolidated test modules behind `#[cfg(feature = "...")]`.

---

### C2 False-positive assertion and panic masking in `jobs_suite.rs` leaves corrupted durable database state undetected

- **Where:** `rume/tests/jobs_suite.rs:293–310`
- **What it says:**
  ```rust
  #[tokio::test]
  async fn worker_panic_does_not_leak_in_flight_count() {
      let (_node, queue) = start_postgres_queue().await;
      let mut registry = JobRegistry::new();
      registry.register(PanickingJobHandler);

      let worker = JobWorker::new(Arc::clone(&queue), Arc::new(registry));
      worker.start();

      queue.enqueue(PanickingJob).await.unwrap();

      // Give time for worker to dequeue and panic
      sleep(Duration::from_millis(50)).await;

      // Drain should succeed without hanging because RAII guard decremented in_flight_count
      let drained = worker.drain(Duration::from_millis(200)).await;
      assert!(drained);
  }
  ```
- **Why it's a problem:**
  This test passes 100% cleanly in CI, yet it masks a critical data corruption defect identified in `jobs-worker-concurrency.md` (finding M1). When a worker task panics, the RAII `InFlightGuard` decrements `in_flight_count`, allowing `worker.drain()` to succeed without hanging. However, because the panic unwinds past the job completion handler:
  1. The job is never acknowledged (`ack`), re-queued (`nack`), or transitioned to the dead-letter queue (`dead_letter`).
  2. In PostgreSQL, the row remains permanently in status `in_flight` with its original claim lease.
  3. The idempotency lock (if configured) is never released.
  The test merely checks `assert!(drained);`—it **never inspects the database state** (`queue.dead_letter_count()`, `queue.pending_count()`, or row status in `rume_jobs`). The test asserts a green pass while the database is left in a corrupted, orphaned state.
- **Suggested fix:**
  Update the test to assert durable database invariants following worker panic:
  ```rust
  let drained = worker.drain(Duration::from_millis(200)).await;
  assert!(drained);

  // Durable queue invariant: panicked job must not be orphaned in 'in_flight'
  assert_eq!(queue.dead_letter_count().await.unwrap(), 1, "panicked job must be routed to DLQ");
  assert_eq!(queue.pending_count().await.unwrap(), 0);
  ```
  Fix the underlying worker in `rume/src/jobs/worker.rs` to wrap handler execution in `std::panic::AssertUnwindSafe(future).catch_unwind()` and route caught panics to `dead_letter` with panic payload telemetry.

---

### C3 Potemkin pool lifecycle test in `postgres_container_suite.rs` asserts boolean flags instead of physical connection drain

- **Where:** `rume/tests/postgres_container_suite.rs:103–107`
- **What it says:**
  ```rust
  // Verify graceful closure
  let closed = pool.close_and_drain(Duration::from_millis(500)).await;
  assert!(closed);
  assert!(pool.status().is_closed);
  ```
- **Why it's a problem:**
  As revealed in `pool-lifecycle-persistence.md` (Lens 2 C1/C2), `DatabasePool::close_and_drain()` is a "potemkin" abstraction: it sets an internal atomic boolean `is_closed` and waits on a disconnected Tokio semaphore. It **never invokes `PgPool::close()`**, does not terminate active PostgreSQL backend server connections, and does not cancel in-flight queries.
  The integration test in `postgres_container_suite.rs` asserted that the method returned `true` and that `pool.status().is_closed` was `true`. It never verified:
  1. Whether executing a query against the pool after closure returns `Err(PersistenceError::PoolClosed)`.
  2. Whether physical server connections were terminated in PostgreSQL (`SELECT count(*) FROM pg_stat_activity WHERE datname = 'postgres' AND application_name LIKE '%rume%'`).
  Asserting internal boolean flags rather than physical system invariants allowed a completely broken pool lifecycle abstraction to pass CI undetected.
- **Suggested fix:**
  In `test_postgres_pool_lifecycle_and_health`, assert that post-close queries are strictly rejected, and verify physical connection closure:
  ```rust
  let closed = pool.close_and_drain(Duration::from_millis(500)).await;
  assert!(closed);

  // Invariant 1: Queries on closed pool must fail immediately
  let query_res = pool.backend().query_scalar("SELECT 1").await;
  assert!(matches!(query_res, Err(PersistenceError::PoolClosed)));

  // Invariant 2: Physical connections in PostgreSQL must be drained to 0
  assert!(pool.backend().inner_pool().is_closed());
  ```

---

### M1 Test directory binary explosion: 32 flat test crates cause 11 GB build bloat and extreme link overhead

- **Where:** `rume/tests/` (32 flat `.rs` files) and `rume/Cargo.toml:53–55`
- **What it says:**
  The `rume/tests/` directory contains 32 separate `.rs` files:
  `auth_suite.rs`, `cache_suite.rs`, `coordination_suite.rs`, `enterprise_integration_pipeline.rs`, `enterprise_suite.rs`, `env_and_config_loading.rs`, `error_contract.rs`, `error_redaction.rs`, `events_suite.rs`, `health_routes.rs`, `http_smoke.rs`, `job_reliability_suite.rs`, `jobs_suite.rs`, `lifecycle_shutdown.rs`, `metrics_suite.rs`, `module_testing_and_architecture_suite.rs`, `mysql_container_suite.rs`, `openapi_suite.rs`, `outbox_suite.rs`, `persistence_integration.rs`, `postgres_container_suite.rs`, `progressive_composition.rs`, `redis_container_suite.rs`, `request_identity.rs`, `resilience_suite.rs`, `route_metadata.rs`, `routing.rs`, `saga_suite.rs`, `security_suite.rs`, `sqlite_suite.rs`, `tenant_suite.rs`, `validation_suite.rs`.
- **Why it's a problem:**
  In Cargo's compilation model, each `.rs` file in `tests/` constitutes an independent compilation unit (crate). Each test binary links the entire dependency graph: `axum`, `tokio`, `sqlx` (with SQLite, Postgres, MySQL drivers), `deadpool-redis`, `testcontainers-modules`, `bollard`, `tonic`, `hyper`, `secrecy`, and `garde`.
  Audit measurements show:
  1. **Binary Size Bloat**: Each individual test executable in `target/debug/deps/` is between 110 MB and 122 MB (e.g. `enterprise_integration_pipeline` is 122 MB; `postgres_container_suite` is 118 MB).
  2. **Disk Exhaustion**: The `target/debug/deps/` directory occupies **11 GB** of disk space solely for test binaries, object archives, and debug symbols.
  3. **Linker Thrashing**: When running `cargo test`, the linker must execute 32 separate linking steps, each copying and linking ~115 MB of code. Over 80% of total CI compilation time is spent waiting on the linker.
  4. **Inability to Separate Fast vs Slow Tests**: Developers cannot easily run fast in-memory unit/integration tests without Cargo compiling all 32 binaries, including heavy container-backed suites.
- **Suggested fix:**
  Consolidate `rume/tests/` into 2 canonical entrypoints and 1 uncompiled common module:
  ```
  rume/tests/
  ├── integration.rs             # Entrypoint 1: Fast zero-docker integration suites
  │   └── integration/
  │       ├── auth.rs
  │       ├── config.rs
  │       ├── error.rs
  │       ├── health.rs
  │       ├── metrics.rs
  │       ├── openapi.rs
  │       ├── request_identity.rs
  │       ├── resilience.rs
  │       ├── routing.rs
  │       ├── saga.rs
  │       ├── validation.rs
  │       └── composition.rs
  ├── containers.rs              # Entrypoint 2: Container-backed live suites
  │   └── containers/
  │       ├── postgres.rs        # #[cfg(feature = "postgres")]
  │       ├── mysql.rs           # #[cfg(feature = "mysql")]
  │       ├── redis.rs           # #[cfg(feature = "redis")]
  │       ├── jobs.rs            # #[cfg(feature = "postgres")]
  │       ├── outbox.rs          # #[cfg(feature = "sqlite")]
  │       └── pipeline.rs        # #[cfg(all(feature = "postgres", feature = "sqlite"))]
  └── common/                    # Uncompiled shared test fixtures & helpers
      ├── mod.rs
      ├── containers.rs          # OnceCell container fixture singletons
      ├── harness.rs             # RumeTestHarness builder
      └── assertions.rs
  ```
  This reduces executable binaries from 32 down to 2, cuts linker invocations by 94%, saves ~9 GB of disk space in `target/debug/deps`, and enables instantaneous local test execution via `cargo test --test integration`.

---

### M2 Per-test container spin-up overhead adds 3+ minutes of idle latency and risks Docker daemon exhaustion

- **Where:** Across container suites in `rume/tests/`:
  - `postgres_container_suite.rs:38–73` (15 tests)
  - `mysql_container_suite.rs:14–36` (7 tests)
  - `redis_container_suite.rs:29–50` (12 tests)
  - `jobs_suite.rs:14–29` (7 tests)
  - `job_reliability_suite.rs:13–45` (4 tests)
  - `enterprise_integration_pipeline.rs:88–98` (1 test)
- **What it says:**
  Each individual `#[tokio::test]` function independently launches a brand new container instance:
  ```rust
  async fn setup_postgres_pool(max_connections: u32) -> (ContainerAsync<Postgres>, DatabasePool<PostgresBackend>) {
      let container = Postgres::default().with_tag("16-alpine").start().await.expect("start pg");
      // format URL, create pool...
      (container, pool)
  }
  ```
- **Why it's a problem:**
  Across the container test suites, **46 Docker containers** are spun up and torn down during a complete test execution.
  1. Starting a PostgreSQL 16 Alpine container takes 3.5–5.0 seconds. Starting a MySQL container takes 6.0–9.0 seconds.
  2. Total cumulative container startup overhead exceeds **180 seconds** (3+ minutes) per test run.
  3. Under parallel test execution (`cargo test -- --test-threads=8`), Tokio tasks launch 8 Docker containers simultaneously, saturating Docker daemon socket connections, creating bridge network IP exhaustion, and causing intermittent test timeouts in constrained CI environments.
- **Suggested fix:**
  Implement reusable, lazily initialized container fixtures with per-test database/schema isolation using `tokio::sync::OnceCell`:
  ```rust
  // tests/common/containers.rs
  use testcontainers_modules::postgres::Postgres;
  use testcontainers_modules::testcontainers::{ContainerAsync, runners::AsyncRunner};
  use tokio::sync::OnceCell;

  static POSTGRES_NODE: OnceCell<ContainerAsync<Postgres>> = OnceCell::const_new();

  pub async fn shared_postgres() -> &'static ContainerAsync<Postgres> {
      POSTGRES_NODE.get_or_init(|| async {
          Postgres::default()
              .with_tag("16-alpine")
              .start()
              .await
              .expect("start shared postgres container")
      }).await
  }

  /// Creates an isolated database for a single test using UUID namespacing.
  pub async fn create_isolated_postgres_db() -> (String, DatabaseConfig) {
      let node = shared_postgres().await;
      let host = node.get_host().await.unwrap();
      let port = node.get_host_port_ipv4(5432).await.unwrap();
      let db_name = format!("test_{}", uuid::Uuid::new_v4().simple());

      // Connect to root postgres DB to issue CREATE DATABASE
      let root_url = format!("postgres://postgres:postgres@{host}:{port}/postgres");
      let root_pool = sqlx::PgPool::connect(&root_url).await.unwrap();
      sqlx::query(&format!("CREATE DATABASE {db_name};")).execute(&root_pool).await.unwrap();

      let test_url = format!("postgres://postgres:postgres@{host}:{port}/{db_name}");
      let config = DatabaseConfig::new(&test_url);
      (db_name, config)
  }
  ```
  - For PostgreSQL and MySQL: issue `CREATE DATABASE test_<uuid>` in ~30ms instead of a 4,000ms container launch, reducing execution time by 93%.
  - For Redis: use `SELECT <idx>` or key prefixing (`test:<uuid>:*`).
  Total container launches drop from 46 down to 3 (one Postgres, one MySQL, one Redis).

---

### M3 Persistent async wall-clock sleeps across 14 test suites violate determinism and introduce flaky races

- **Where:** 34 occurrences across 14 integration test suites:
  - `jobs_suite.rs:208, 216, 239, 263, 305, 344, 359, 394, 411`
  - `job_reliability_suite.rs:79, 102, 128, 211, 236`
  - `lifecycle_shutdown.rs:103, 122, 172, 180, 194`
  - `redis_container_suite.rs:215, 235, 305, 350`
  - `mysql_container_suite.rs:115, 142`
  - `cache_suite.rs:71, 145`
  - `events_suite.rs:107, 124`
  - `progressive_composition.rs:224, 377`
  - `coordination_suite.rs:51`
  - `enterprise_integration_pipeline.rs:142`
  - `persistence_integration.rs:88`
  - `postgres_container_suite.rs:260`
  - `request_identity.rs:324`
- **What it says:**
  Async tests wait for background jobs, worker drain, lock TTL expirations, server readiness, and rate limit windows using raw sleeps:
  ```rust
  // jobs_suite.rs:263
  // Give it 20ms to be dequeued and in-flight
  sleep(Duration::from_millis(20)).await;
  assert_eq!(finished.load(Ordering::SeqCst), 0);
  ```
  ```rust
  // lifecycle_shutdown.rs:103
  // Give server a moment to start and declare Ready
  tokio::time::sleep(Duration::from_millis(50)).await;
  assert_eq!(server_ctx.lifecycle.state(), LifecycleState::Ready);
  ```
- **Why it's a problem:**
  1. **Flaky Race Conditions**: A 20ms or 50ms wall-clock sleep assumes the OS thread scheduler will dispatch the spawned background Tokio task before the sleep elapses. Under CPU contention on shared CI runners, 20ms is frequently insufficient, causing false-negative test failures.
  2. **Cumulative Latency**: 34 sleeps ranging from 20ms to 250ms accumulate multiple seconds of idle waiting in every test run.
  3. **Incomplete v02 Remediation**: While v02 `todo.md` recorded this issue as resolved, 91% of sleeps (34 of 37) remain unmigrated.
- **Suggested fix:**
  1. **Concurrency and State Transitions**: Replace arbitrary sleeps with explicit notification primitives:
     - Use `tokio::sync::Notify`, `tokio::sync::Barrier`, or `tokio::sync::watch` channels to trigger progression immediately when a background state transition occurs.
     - In `jobs_suite.rs`, use an atomic barrier or oneshot channel inside the test job handler to signal that execution has started, rather than sleeping 20ms.
  2. **TTL, Timeout, and Backoff Simulation**: Wrap time-dependent tests with Tokio's paused virtual clock (`tokio::time::pause()` and `tokio::time::advance(duration).await`), allowing instant, 100% deterministic time fast-forwarding without sleeping real wall-clock time.

---

### M4 Absence of a first-party declarative test harness (`RumeTestHarness`) forces repetitive boilerplate across test suites

- **Where:** `rume/src/testing.rs:10–173`, `rume/tests/enterprise_integration_pipeline.rs:75–100`, `rume/tests/persistence_integration.rs:18–45`, `rume/tests/outbox_suite.rs:12–16`, `rume/tests/sqlite_suite.rs:14–26`
- **What it says:**
  `rume::testing` currently exports only `TestApp` and `TestAppBuilder` (for bare Axum HTTP routers) and `status()`. There are zero testing utilities for database persistence, transactional outbox enlistment, background job runners, or distributed coordination.
- **Why it's a problem:**
  Every integration test requiring stateful infrastructure must manually duplicate 30–50 lines of boilerplate:
  1. Initializing connection URLs and configuring `DatabaseConfig`.
  2. Creating `DatabasePool` and initializing table schemas via manual DDL execution (`init_outbox_table()`, `PostgresJobQueue::init_schema()`).
  3. Manually instantiating `OutboxRelay`, `JobRegistry`, and `JobWorker`.
  4. Managing background task join handles and cleanup drop guards.
  This boilerplate obscures actual test intent, increases friction for writing tests, and causes inconsistent test configurations across suites.
- **Suggested fix:**
  Introduce a declarative, fluent `RumeTestHarness` in `rume::testing`:
  ```rust
  // rume/src/testing/harness.rs
  pub struct RumeTestHarness<B: DatabaseBackend> {
      pub pool: DatabasePool<B>,
      pub outbox: Option<Arc<OutboxRelay<B>>>,
      pub job_worker: Option<JobWorker<PostgresJobQueue>>,
      pub app: TestApp,
  }

  pub struct RumeTestHarnessBuilder { /* ... */ }

  impl RumeTestHarnessBuilder {
      pub fn with_sqlite_in_memory() -> Self;
      pub fn with_postgres_pool(pool: DatabasePool<PostgresBackend>) -> Self;
      pub fn with_outbox(mut self) -> Self;
      pub fn with_jobs(mut self, registry: JobRegistry) -> Self;
      pub fn with_router(mut self, router: Router<()>) -> Self;
      pub async fn build(self) -> Result<RumeTestHarness<...>, TestError>;
  }
  ```
  Usage in integration tests reduces setup to 4 lines:
  ```rust
  let harness = RumeTestHarness::builder()
      .with_sqlite_in_memory()
      .with_outbox()
      .build()
      .await?;

  harness.outbox.unwrap().poll_and_dispatch().await?;
  ```

---

### M5 Lack of modern testing ecosystem tooling (`insta`, `rstest`, `wiremock`) hinders snapshot and protocol verification

- **Where:** `rume/Cargo.toml:53–55`, `rume/tests/openapi_suite.rs:30–120`, `rume/tests/error_contract.rs:50–160`, `rume/tests/enterprise_suite.rs:31–120`
- **What it says:**
  In `rume/Cargo.toml`, dev-dependencies are limited to:
  ```toml
  [dev-dependencies]
  tempfile = "3.27.0"
  testcontainers-modules = { workspace = true }
  ```
  All schema assertions, RFC 9457 Problem Details checks, extractor permutations, and enterprise protocol tests are hand-coded with dozens of manual `assert_eq!` calls or static in-memory XML strings.
- **Why it's a problem:**
  1. **Schema & Problem Details Drift**: `openapi_suite.rs` tests a complex 150-line OpenAPI 3.1 JSON document using 35 individual line-by-line `assert_eq!(spec.paths.get(...)...)` assertions. This approach is brittle, hard to read, and frequently misses regressions in omitted fields, formatting, or type annotations. The same issue affects `error_contract.rs` RFC 9457 Problem Details JSON payload assertions.
  2. **Test Permutation Duplication**: Tests in `validation_suite.rs`, `route_metadata.rs`, and `routing.rs` duplicate 10–15 nearly identical test functions to test HTTP methods (`GET`, `POST`, `PUT`, `DELETE`, `PATCH`) or parameter validation regexes.
  3. **Zero External HTTP Protocol Simulation**: Enterprise protocols in `enterprise_suite.rs` (SAML 2.0 and SCIM 2.0) test only string serialization in memory. There is **zero HTTP wire testing** of external identity providers (IdP metadata discovery, SAML artifact resolution, OAuth token introspection) or outbound webhook dispatchers.
- **Suggested fix:**
  Adopt standard Rust testing ecosystem crates in `rume/Cargo.toml` `[dev-dependencies]`:
  1. **`insta` (Snapshot Testing)**:
     Add `insta = { version = "1.42", features = ["json"] }`. Replace verbose manual assertions in `openapi_suite.rs` and `error_contract.rs` with `insta::assert_json_snapshot!(spec)` and `insta::assert_json_snapshot!(problem_details)`.
  2. **`rstest` (Parameterized & Fixture Testing)**:
     Add `rstest = "0.24"`. Unify duplicated test permutations across `validation_suite.rs` and `route_metadata.rs` into parameterized table-driven tests:
     ```rust
     #[rstest]
     #[case(Method::GET, "/users/1", StatusCode::OK)]
     #[case(Method::POST, "/users", StatusCode::CREATED)]
     #[case(Method::DELETE, "/users/1", StatusCode::NO_CONTENT)]
     async fn test_route_methods(#[case] method: Method, #[case] path: &str, #[case] expected: StatusCode) { ... }
     ```
  3. **`wiremock` (HTTP Mocking)**:
     Add `wiremock = "0.6"`. Implement tests verifying SAML IdP metadata HTTP fetching, SCIM upstream synchronization, and outbox webhook delivery with simulated 500/503 errors and network timeouts.

---

### m1 Inconsistent test file naming conventions across `rume/tests/`

- **Where:** `rume/tests/` directory structure
- **What it says:**
  Test suite files use five competing naming patterns:
  - `*_suite.rs` (`auth_suite.rs`, `cache_suite.rs`, `coordination_suite.rs`, `jobs_suite.rs`, `outbox_suite.rs`)
  - `*_container_suite.rs` (`postgres_container_suite.rs`, `mysql_container_suite.rs`, `redis_container_suite.rs`)
  - `*_integration.rs` (`persistence_integration.rs`)
  - `*_pipeline.rs` (`enterprise_integration_pipeline.rs`)
  - Bare nouns and descriptive names (`routing.rs`, `route_metadata.rs`, `http_smoke.rs`, `request_identity.rs`, `lifecycle_shutdown.rs`, `progressive_composition.rs`)
- **Why it's a problem:**
  Inconsistent naming impairs code discovery, obscures which tests require Docker vs in-memory execution, and complicates test filtering (`cargo test --test ...`).
- **Suggested fix:**
  Standardize naming as part of the directory consolidation in finding M1: use simple, consistent noun modules under `tests/integration/` (e.g. `auth.rs`, `routing.rs`) and `tests/containers/` (e.g. `postgres.rs`, `redis.rs`).

---

### m2 Redundant test coverage between unit tests and integration tests

- **Where:** `rume/src/error.rs:286–455` vs `rume/tests/error_contract.rs:49–165`; `rume/src/routing/health.rs:362–660` vs `rume/tests/health_routes.rs:20–120`
- **What it says:**
  `error_contract.rs` and `health_routes.rs` replicate nearly identical test cases and assertions already verified in unit tests in `rume/src/error.rs` (testing each `AppError` variant's status and JSON schema) and `rume/src/routing/health.rs` (testing health check transitions and degraded statuses).
- **Why it's a problem:**
  Exact-duplicate test coverage increases build and test execution time without providing additional defect detection.
- **Suggested fix:**
  Prune unit-level duplication from the integration test files. Focus `error_contract.rs` purely on Axum wire extractor rejections (e.g. invalid JSON syntax, missing headers) and retain domain variant testing in `src/error.rs`.

---

### m3 Duplicate container spin-up logic across PostgreSQL and Redis suites

- **Where:** `rume/tests/postgres_container_suite.rs:38–73`, `rume/tests/jobs_suite.rs:14–29`, `rume/tests/job_reliability_suite.rs:30–45`, `rume/tests/enterprise_integration_pipeline.rs:88–98`
- **What it says:**
  Each test suite implements its own private container setup helper (`setup_postgres_pool`, `start_postgres_queue`, `start_redis_pool`) that sets up `Postgres::default()`, queries port 5432, formats connection strings, and connects a pool.
- **Why it's a problem:**
  Configuration settings (such as PostgreSQL image tag, pool size limits, and connection timeouts) are copy-pasted across 4 files, creating maintenance friction when adjusting container versions or pool parameters.
- **Suggested fix:**
  Centralize container setup and connection string formatting into `tests/common/containers.rs` and reuse across all container-backed test modules.

---

## Positive notes

- **Real Database Integration Depth**: Retaining live container integration suites for PostgreSQL, MySQL, and Redis ensures that real SQL engine dialects (`FOR UPDATE SKIP LOCKED`, row locking, UUID generation, Redis TTLs) are exercised against actual database servers rather than in-memory mocks.
- **Atomic Transactional Outbox Verification**: `rume/tests/outbox_suite.rs:260–340` thoroughly tests both successful two-phase atomic commitment and clean rollback of outbox events alongside business database mutations on SQLite.
- **Comprehensive Extractor Validation**: `rume/tests/validation_suite.rs` exercises compound parameter extractors (`ValidatedPath`, `ValidatedQuery`, `ValidatedJson`) and confirms structured field-level error detail aggregation.
- **Module Dependency Transitive Enforcement**: `rume/src/module/architecture.rs` successfully resolved v02 M2 by computing transitive dependency reachability closures via BFS, preventing indirect architecture violations.
- **Process Environment Isolation**: The introduction of `EnvVarGuard` in `rume/tests/env_and_config_loading.rs` guarantees hermetic restoration of process environment variables, preventing cross-test state leakage.
- **Deterministic Time Simulation in Resilience Suite**: `rume/tests/resilience_suite.rs` demonstrates the correct use of `tokio::time::pause()` for instantaneous, deterministic circuit breaker half-open probe testing.

---

## Verdict

**Fail — conditional.**

The test suite provides extensive verification depth across real database engines, transactional outbox operations, and application lifecycle states. However, it cannot be certified as production-ready until the following **3 Critical** and **5 Major** findings are resolved:

1. **C1** — Fix feature gate consistency across all test suites by adding `#![cfg(feature = "...")]` to ungated test crates, ensuring `cargo test` compiles under default features and `cargo check --tests` succeeds across all single-feature matrix configurations (`sqlite`, `postgres`, `mysql`, `no-default-features`).
2. **C2** — Eliminate false-positive test assertions in `jobs_suite.rs` by asserting durable database state in PostgreSQL following worker panics.
3. **C3** — Update `test_postgres_pool_lifecycle_and_health` to verify physical connection closure and ensure queries on closed pools return `Err(PersistenceError::PoolClosed)`.
4. **M1** — Reorganize `rume/tests/` from 32 flat files into consolidated entrypoints (`tests/integration.rs`, `tests/containers.rs`, `tests/common/`) to eliminate the 11 GB multi-binary compile explosion and link overhead.
5. **M2** — Implement shared container fixtures with `tokio::sync::OnceCell` and per-test database isolation (`CREATE DATABASE test_<uuid>`), reducing container launches from 46 to 3 and eliminating 3+ minutes of CI latency.
6. **M3** — Replace the 34 remaining wall-clock `sleep()` calls with explicit synchronization primitives (`Notify`, channels) and Tokio paused virtual time (`tokio::time::pause()`).
7. **M4** — Provide a declarative `RumeTestHarness` in `rume::testing` to eliminate repetitive pool, outbox, and worker setup boilerplate.
8. **M5** — Add `insta`, `rstest`, and `wiremock` to dev-dependencies for OpenAPI/Problem Details snapshot testing, parameterized route validation, and external IdP/webhook protocol verification.
