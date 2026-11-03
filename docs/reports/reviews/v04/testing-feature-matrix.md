# Testing & Feature Matrix Review — v04

## Summary

Rume's testing infrastructure and build matrix verification have achieved significant structural maturation since v03: the 2-binary architecture (`tests/integration.rs` and `tests/containers.rs`) eliminates multi-binary linker thrashing; single-feature matrix builds (`--no-default-features`, `--features sqlite|postgres|mysql|redis`, and `--all-features`) compile cleanly; all 15 live Redis containers that previously contaminated the "zero-docker" integration suite were eliminated via in-memory fixture isolation; the four deterministic test failures from v03/diff/v01 are resolved; and the end-to-end CLI scaffold suite compiles and passes. However, the testing surface is **not ready for production certification** due to four Major defects: (1) severe regression in test determinism, with 33 wall-clock `sleep()` calls persisting across 10 test suites (adding >12s of idle latency and sleeping up to 1,200ms in in-memory SQLite integration tests and 2,400ms in containers); (2) `RumeTestHarness` is completely orphaned with zero adoption across `tests/`, lacks background job/app integration, and emits compiler warnings on non-sqlite builds; (3) `wiremock` is used only in a tautological test asserting `wiremock` against `ureq` with zero Rume code exercised; and (4) critical negative-path failure modes remain unasserted across newly introduced event bus listener panics, saga intermediate compensation failures, and job worker lease theft.

---

## Prior decisions

Every prior finding across `v01/testing.md`, `v02/testing.md`, `v03/testing-feature-matrix.md`, and `v03/diff/v01/testing-feature-matrix.md` has been audited against the v04 codebase:

- **v01 T1 (`routing.rs` has zero unit tests)** — **REAFFIRMED**. Verified: all 7 submodules in `rume/src/routing/` carry comprehensive hermetic unit tests (`group.rs:177`, `health.rs:362`, `matchers.rs:87`, `registry.rs:133`, `rejection.rs:45`, `resource.rs:126`, `validated_param.rs:89`), and integration suites in `rume/tests/integration/` (`routing.rs`, `route_metadata.rs`, `health.rs`) test Axum dispatch end-to-end. Executed: `cargo test -p rume --test integration -- routing route_metadata health` → 22 passed, 0 failed.
- **v01 T2 (No fails-before proof anywhere in the suite)** — **REAFFIRMED**. While regression test assertions exist for known bugs, committed fails-before artifacts or `todo_*` reproduction harnesses remain absent in repository history.
- **v01 T3 (`config.rs` entirely untested)** — **REAFFIRMED**. Verified: `rume/src/config.rs:259–325` contains unit tests for raw TOML loading, precedence, and missing files using isolated `tempfile::tempdir()`.
- **v01 T4 (`TestApp`/`TestAppBuilder` test harness is itself untested)** — **REAFFIRMED**. Verified: `rume/src/testing/app.rs:151–268` contains 7 unit tests explicitly labeled `T-T4` testing HTTP request dispatch, custom state, status helpers, and cleanup drop guards.
- **v01 T5 (`app.rs` tests miss boundary, Inspect-mode, lifecycle-ordering surface)** — **REAFFIRMED**. Verified: `rume/src/app/state.rs:205–234` verifies out-of-range memory reads map deterministically to `Created`/`Unknown`; `rume/src/app/builder.rs:1381–1440` verifies `ApplicationMode::Inspect` never binds listeners or executes hooks.
- **v01 T6 (`error.rs` tests cover 3 of 6 variants and assert no response bodies)** — **REAFFIRMED**. Verified: `rume/src/error.rs:286–455` tests all 7 variants, asserts RFC 9457 Problem Details JSON schemas, and confirms diagnostic redaction.
- **v01 T7 (`env.rs` has no `file:` traversal test, and integration env tests mutate global process env)** — **REAFFIRMED in part, CHALLENGED in part**. Path traversal is thoroughly rejected and tested in `rume/src/env/loading.rs:300–319`. However, `rume/tests/integration/config.rs:12–53` still uses an `EnvVarGuard` that calls `unsafe { std::env::set_var }` / `remove_var`, requiring `#![allow(unsafe_code)]` and preventing `tests/integration.rs` from enforcing `#![forbid(unsafe_code)]` (see **m3**).
- **v01 T8 (`http_smoke.rs` tests a bare router, not real health surface)** — **REAFFIRMED**. Dedicated integration tests in `rume/tests/integration/health.rs` verify `health_routes()` with `AppContext` state and availability transitions.
- **v01 T9 (No test isolates or cleans up `ROUTE_REGISTRY` global state across tests)** — **REAFFIRMED**. Global state was eliminated; `RouteRegistry` is embedded directly in `AppContext` and `Router`.
- **v02 M1 (Happy-path test bias across newly introduced subsystems fails to verify failure modes)** — **CHALLENGED (Recurred in v04 Subsystems)**. Negative-path test coverage was added for outbox lease theft (`integration/outbox.rs:400–500`), job timeouts and worker panics (`containers/jobs.rs:318, 374`), and SAML signature wrapping (`integration/enterprise.rs:104, 270`). However, happy-path bias recurred in newly scoped subsystems: background listener panics and errors are unasserted in `events.rs`, intermediate compensation failures and step panics are unasserted in `saga.rs`, and wiremock testing is purely tautological (see **M3**, **M4**).
- **v02 M2 (`ArchitectureRules::verify` only inspects direct dependencies)** — **REAFFIRMED in part, CHALLENGED in part**. BFS transitive closure verification was implemented in `rume/src/module/architecture.rs:84–120` and tested in `rume/tests/integration/architecture.rs`. However, as identified in Lens 6 M1, undeclared modules and circular dependencies still bypass `verify()`.
- **v02 M3 / v03 M3 / diff M4 (Widespread non-deterministic wall-clock sleeps across async tests)** — **CHALLENGED (Severe Regression)**. Rather than progressing towards zero sleeps, raw wall-clock sleeps have ballooned to **33 occurrences** across 10 test suites, including 1,200ms sleeps in in-memory SQLite integration tests and up to 2,400ms sleeps in container suites. Filed as **M1**.
- **v02 M4 (`TestApp` and `RumeBuilder::test_app()` bypass module lifecycle hooks)** — **REAFFIRMED in part, CHALLENGED in part**. Asynchronous `build_test_app()` was introduced in `rume/src/app/builder.rs:567–697`. However, as identified in Lens 7 M2, teardown ordering inverts shutdown hooks relative to `run()`.
- **v02 m1 (`ModuleHarness` lacks health check verification)** — **REAFFIRMED**. Verified in `rume/src/module/harness.rs:76–95` and `rume/tests/integration/architecture.rs:41–67`.
- **v02 m2 (Process environment mutation in config tests is not hermetically restored)** — **REAFFIRMED**. RAII `EnvVarGuard` in `rume/tests/integration/config.rs:12–53` automatically unsets or restores variables upon drop.
- **v02 m3 (Config unit tests use hardcoded `/tmp` paths)** — **REAFFIRMED**. Verified in `rume/src/config.rs:259–325` (`tempfile::tempdir()`).
- **v03 C1 / diff C1 (Broken feature matrix: ungated backend dependencies break `cargo check`/`test`)** — **REAFFIRMED (Remediation verified)**. In `rume/tests/common/containers.rs:6–11`, `OnceCell`, `AsyncRunner`, `ContainerAsync`, and `ImageExt` are correctly gated behind `#[cfg(any(feature = "postgres", feature = "mysql", feature = "redis"))]`. Executed: `cargo check -p rume --tests --no-default-features` and single-feature checks for `sqlite`, `postgres`, `mysql`, `redis`, and `--all-features` all compile cleanly.
- **v03 C2 (False-positive assertion and panic masking in `jobs_suite.rs`)** — **REAFFIRMED (Remediation verified)**. In `rume/tests/containers/jobs.rs:318–342`, the test asserts `queue.dead_letter_count().await == 1` and `queue.pending_count().await == 0`. With `AssertUnwindSafe(fut).catch_unwind().await` in `JobWorker`, the test passes deterministically. Executed: `cargo test -p rume --all-features --test containers -- worker_panic_does_not_leak_in_flight_count` → passed.
- **v03 C3 / diff M3 (Potemkin pool lifecycle test in `postgres_container_suite.rs`)** — **REAFFIRMED (Remediation verified)**. In `rume/tests/containers/postgres.rs:82–101`, `test_postgres_pool_lifecycle_and_health` asserts `pool.backend().is_closed()`, verifies `pool.acquire_lease()` returns `Err(PersistenceError::Closed)`, and verifies `pool.backend().execute("SELECT 1").await` fails with `Err(PersistenceError::Closed)`. Executed: test passed.
- **v03 M1 (Test directory binary explosion: 32 flat test crates cause 11 GB build bloat)** — **REAFFIRMED (Remediation verified)**. 32 flat binaries remain consolidated into 2 canonical test entrypoints: `tests/integration.rs` and `tests/containers.rs` (`rume/Cargo.toml:63–69`).
- **v03 M2 / diff M1 (Contamination of "zero-docker" `tests/integration.rs` with 15 unshared live Redis container spin-ups)** — **REAFFIRMED (Remediation verified)**. All 15 live Redis container spin-ups were removed from `tests/integration/` (`auth.rs`, `cache.rs`, `tenant.rs` now use `InMemoryCacheBackend`, `InMemorySessionStore`, and `InMemoryTenantQuotaStore`). Live Redis tests reside exclusively in `tests/containers/` (`redis.rs`, `coordination.rs`). Executed: `cargo test -p rume --test integration` executes 166 tests in 1.25s with zero Docker interaction.
- **v03 M4 (Absence of a first-party declarative test harness `RumeTestHarness`)** — **CHALLENGED (Defective / Orphaned Remediation)**. While `RumeTestHarness` was added in `rume/src/testing/harness.rs`, it has **zero usage across the test suite** (`grep -rn "RumeTestHarness" rume/tests/` yields 0 hits), is overly constrained, and triggers compiler warnings when `sqlite` is disabled. Re-reported as **M2**.
- **v03 M5 (Lack of modern testing ecosystem tooling `insta`, `rstest`, `wiremock`)** — **REAFFIRMED in part, CHALLENGED in part**. Tooling is declared in `Cargo.toml`. However, `wiremock` is only used in a tautological test exercising zero Rume code (see **M3**), `insta` is duplicated by 28 manual `assert_eq!` assertions in `openapi.rs` (see **m2**), and `rstest` is used in only 1 test in `routing.rs`.
- **v03 m1 (Inconsistent test file naming conventions)** — **REAFFIRMED**. Standardized noun modules under `integration/` and `containers/` are maintained.
- **v03 m2 / diff m1 (Redundant test coverage between unit and integration tests)** — **REAFFIRMED in part, CHALLENGED in part**. `integration/health.rs` properly exercises the Axum HTTP routing layer, but `integration/openapi.rs` and `integration/error.rs` still duplicate manual JSON assertions (see **m2**).
- **v03 m3 (Duplicate container spin-up logic across PostgreSQL and Redis)** — **REAFFIRMED**. Shared container singletons and isolated database creators are centralized in `rume/tests/common/containers.rs`.
- **diff M2 (Container test suite regressions: 4 deterministic test failures)** — **REAFFIRMED (Remediation verified)**. All 4 failing tests (`test_postgres_migrations_up_and_rollback`, `worker_panic_does_not_leak_in_flight_count`, `test_worker_heartbeat_and_abandoned_job_recovery`, `test_redis_idempotency_store`) pass cleanly. Executed: `cargo test -p rume --all-features --test containers -- ...` → 4 passed, 0 failed.
- **diff m2 (Unused import compiler warnings in `rume/src/testing/harness.rs`)** — **CHALLENGED (Unresolved under single-feature builds)**. Compiling `rume` tests under `--no-default-features` or any configuration without `sqlite` emits 3 unused import warnings in `harness.rs:105–107`. Re-reported in **M2**.

---

## Findings

### M1 Severe regression in test determinism: 33 wall-clock `sleep()` calls persist across 10 test suites, adding idle latency and flaky race hazards

- **Where:** 33 occurrences across `rume/tests/`:
  - `rume/tests/integration/outbox.rs:931, 1011` (`tokio::time::sleep(Duration::from_millis(1200)).await;`)
  - `rume/tests/integration/outbox.rs:1069` (`tokio::time::sleep(Duration::from_millis(20)).await;`)
  - `rume/tests/integration/cache.rs:159, 201` (`Duration::from_millis(300)`, `Duration::from_millis(30)`)
  - `rume/tests/integration/lifecycle.rs:182, 190` (`Duration::from_millis(30)`, `Duration::from_millis(250)`)
  - `rume/tests/containers/postgres.rs:1160, 1179, 1198, 1227, 1266, 1273, 1307, 1344, 1393, 1442, 1444, 1489` (sleeps of 150ms, 250ms, 500ms, 600ms, 900ms, 1600ms, and **2400ms**)
  - `rume/tests/containers/pipeline.rs:458, 663, 734` (`Duration::from_millis(250)`, `1200`, `1700`)
  - `rume/tests/containers/redis.rs:109, 340, 479, 581` (`Duration::from_millis(150)`, `250`, `550`, `1100`)
  - `rume/tests/containers/job_reliability.rs:114, 257, 278` (`Duration::from_millis(10)`, `60`, `150`)
  - `rume/tests/containers/jobs.rs:258, 390` (`Duration::from_millis(5)`, `job.duration_ms`)
  - `rume/tests/containers/coordination.rs:59, 101` (`Duration::from_millis(100)`, `250`)
- **What it says:**
  Tests wait for background state transitions, lease renewals, heartbeat timeouts, and cache expirations using raw wall-clock sleeps up to 2.4 seconds:
  ```rust
  // rume/tests/integration/outbox.rs:927-936
  // Dispatcher runs for 1200ms (> initial 1s lease)
  let relay = OutboxRelay::new(Arc::clone(&repo), move |_msg: &OutboxMessage| {
      let count = Arc::clone(&dispatched_clone);
      async move {
          tokio::time::sleep(Duration::from_millis(1200)).await;
          count.fetch_add(1, Ordering::SeqCst);
          Ok(())
      }
  })
  .with_config(config);
  ```
  ```rust
  // rume/tests/containers/postgres.rs:1390-1398
  // Dispatcher sleeps 2400ms (> initial 2s lease duration!)
  let relay = OutboxRelay::new(Arc::clone(&backend), move |_msg: &OutboxMessage| {
      let count = Arc::clone(&dispatched_clone);
      async move {
          tokio::time::sleep(Duration::from_millis(2400)).await;
          count.fetch_add(1, Ordering::SeqCst);
          Ok(())
      }
  })
  ```
- **Why it's a problem:**
  1. **Contaminates the "fast zero-docker" suite**: `rume/tests/integration.rs` advertises fast in-memory execution, yet two tests (`integration/outbox.rs:931, 1011`) sleep for 1,200ms of real wall-clock time each. In our executed run, `tests/integration.rs` took 1.25s, of which **1.20s (>95%) was idle wall-clock sleeping**.
  2. **Massive container suite latency**: Across `tests/containers/`, cumulative sleep time exceeds **12 seconds** per run (e.g. lines 1393 and 1489 in `postgres.rs` sleep for 2,400ms each; line 734 in `pipeline.rs` sleeps for 1,700ms; line 581 in `redis.rs` sleeps for 1,100ms).
  3. **Flaky race conditions**: Arbitrary sleeps of 10ms–60ms (`job_reliability.rs:257, 278`, `jobs.rs:390`, `outbox.rs:1069`) assume the Tokio runtime scheduler will dispatch background tasks before the timer elapses. Under CPU contention on shared CI runners, short sleeps frequently fail, causing false-negative test failures.
  4. **Neglect of virtual time**: In-memory TTL tests in `cache.rs:159` and `lifecycle.rs:182` can use Tokio's virtual paused time (`tokio::time::pause()`, `advance()`) or `tokio::sync::Notify` for instant, deterministic execution without burning real time.
- **Suggested fix:**
  1. For state transition tests, replace sleeps with `tokio::sync::Notify` or `tokio::sync::watch` channels to trigger immediately when the condition is met.
  2. For in-memory TTL simulation in `integration/cache.rs` and `integration/lifecycle.rs`, wrap tests with `tokio::time::pause()` and `tokio::time::advance(duration).await`.
  3. For heartbeat renewal tests in `outbox.rs` and `postgres.rs`, allow sub-second lease durations in `OutboxRelayConfig` (e.g. `lease_duration: Duration` instead of `lease_duration_secs: u32`) so lease tests can run with 50ms leases and 80ms dispatches instead of multi-second sleeps.

---

### M2 Disconnected and orphaned test harness: `RumeTestHarness` has zero adoption across `tests/`, lacks background job/app integration, and triggers compiler warnings

- **Where:** `rume/src/testing/harness.rs:1–129` and `rume/tests/` (0 consumers)
- **What it says:**
  `RumeTestHarness` was added in `rume/src/testing/harness.rs` to eliminate repetitive test boilerplate:
  ```rust
  pub struct RumeTestHarness<B: crate::persistence::DatabaseBackend + crate::outbox::OutboxRepository> {
      pub pool: crate::persistence::DatabasePool<B>,
      pub outbox: Option<std::sync::Arc<crate::outbox::OutboxRelay<crate::persistence::DatabasePool<B>>>>,
      pub app: Option<TestApp>,
  }
  ```
  However:
  1. Running `grep -rn "RumeTestHarness" rume/tests/` returns **0 occurrences**. Across all 25 integration modules and 7 container modules, not a single test uses `RumeTestHarness`. Every test continues to write 25–40 lines of manual pool creation, schema initialization (`init_outbox_table()`), and relay wiring boilerplate.
  2. The harness struct enforces `B: DatabaseBackend + OutboxRepository`. This tightly couples pure database persistence tests to outbox repositories, and completely excludes background jobs (`JobQueue`, `JobWorker`).
  3. It only wraps a bare `axum::Router<()>`, lacking any integration with `AppContext` or `RumeBuilder::build_test_app()`.
  4. Lines 104–108 define:
     ```rust
     #[cfg(test)]
     mod tests {
         use super::*;
         use axum::http::StatusCode;
         use axum::routing::get;

         #[cfg(feature = "sqlite")]
         #[tokio::test]
         async fn test_rume_test_harness_in_memory_lifecycle() { ... }
     }
     ```
     Because the test is gated with `#[cfg(feature = "sqlite")]`, building tests under `--no-default-features` or any single-feature configuration without `sqlite` (`postgres`, `mysql`, `redis`) leaves `super::*`, `StatusCode`, and `get` unused, emitting 3 compiler warnings.
- **Why it's a problem:**
  The harness is "scaffolding without wiring". It did not solve the problem it was created for (reducing boilerplate in `tests/`), provides an overly narrow API, and causes diagnostic clutter on CI builds.
- **Suggested fix:**
  1. Decouple `RumeTestHarness` so `outbox` is conditional on `B: OutboxRepository` rather than a struct-level bound on `B`.
  2. Add background job runner support (`.with_jobs(registry)`) and `AppContext` binding.
  3. Adopt `RumeTestHarness` across `tests/integration/persistence.rs`, `tests/integration/outbox.rs`, and `tests/integration/sqlite.rs`.
  4. Gate `mod tests` or its imports behind `#[cfg(feature = "sqlite")]` to eliminate the 3 compiler warnings.

---

### M3 Tautological third-party mock test in `integration/enterprise.rs`: `wiremock` test exercises zero Rume code

- **Where:** `rume/tests/integration/enterprise.rs:300–315`
- **What it says:**
  ```rust
  #[tokio::test]
  async fn test_enterprise_idp_metadata_wiremock_integration() {
      let mock_server = wiremock::MockServer::start().await;
      let metadata_xml = r#"<EntityDescriptor entityID="https://mock-idp.example.com"><IDPSSODescriptor /></EntityDescriptor>"#;

      wiremock::Mock::given(wiremock::matchers::method("GET"))
          .and(wiremock::matchers::path("/saml/metadata"))
          .respond_with(wiremock::ResponseTemplate::new(200).set_body_string(metadata_xml))
          .mount(&mock_server)
          .await;

      let target_url = format!("{}/saml/metadata", mock_server.uri());
      let mut resp = ureq::get(&target_url).call().expect("fetch IdP metadata from mock server");
      assert_eq!(resp.status(), 200);
      let body = resp.body_mut().read_to_string().expect("read body");
      assert!(body.contains("mock-idp.example.com"));
  }
  ```
- **Why it's a problem:**
  This test was added to claim resolution of v03 M5 ("wiremock for IdP/webhooks"). However, inspecting the test body reveals that **not a single type, function, or method from `rume` is called**:
  - It mounts a mock on `wiremock::MockServer`.
  - It issues an HTTP request using third-party `ureq::get()`.
  - It asserts `wiremock` returned 200 and the mock body.
  Rume does not currently contain an IdP metadata HTTP client in `src/enterprise/saml.rs`. The test merely verifies that `wiremock` and `ureq` function as third-party libraries, providing zero verification of Rume's actual codebase. This directly violates `.opencode/rules/testing.md` ("A mock earns no assertions of its own: assert the real component's behavior, and if the mock is what a test is checking, unmock it or delete the assertion").
- **Suggested fix:**
  Either:
  1. Implement the real IdP metadata fetcher / outbox webhook dispatcher in Rume (`rume::enterprise::saml::fetch_idp_metadata` or `rume::outbox::webhook::HttpWebhookDispatcher`) and verify that component against the `wiremock` server; OR
  2. Delete this Potemkin test and remove `ureq` from `[dev-dependencies]` until an actual HTTP-outbound client exists in `rume`.

---

### M4 Missing negative-path and failure-mode coverage across newly introduced subsystems (events, saga, and jobs lease fencing)

- **Where:**
  - `rume/tests/integration/events.rs:97–129`
  - `rume/tests/integration/saga.rs:171–223`
  - `rume/tests/containers/jobs.rs:73–480`
- **What it says:**
  The test suites for newly scoped Phase 4/5 subsystems exhibit severe happy-path bias:
  1. *Event Bus (`events.rs`)*: Tests cover synchronous dispatch and clean asynchronous notification. There are **zero tests** asserting behavior when an asynchronous listener panics or returns an error. This blindspot allowed Lens 1 M1 (background listener errors silently dropped with zero panic isolation) to pass CI undetected.
  2. *Saga Orchestration (`saga.rs`)*: `test_saga_compensation_failure` tests only a 2-step saga where step 2 fails and step 1's compensation fails. There are **zero tests** verifying multi-step cascades ($\ge 3$ steps) where an intermediate step's compensation fails, allowing Lens 1 M2 (partial rollback aborts on the first compensation error and abandons all remaining compensations) to pass CI unnoticed. Furthermore, there are **zero tests** for saga step panics (Lens 1 M3).
  3. *Background Jobs (`containers/jobs.rs`)*: While `containers/postgres.rs` has tests for lease expiry, `containers/jobs.rs` has **zero tests** verifying that a superseded worker attempting to `ack`, `nack`, or `dead_letter` a job whose lease was stolen or expired is rejected with `rows_affected == 0`, or that connection drop mid-handler routes to DLQ.
- **Why it's a problem:**
  Passing tests on benign inputs do not prove system correctness. The exact failure modes and data corruption risks identified in Lens 1 were invisible to CI because the test suite never exercised negative paths.
- **Suggested fix:**
  1. In `tests/integration/events.rs`: add tests asserting that panicking event listeners do not crash the runtime, and that async handler errors are logged or routed to an error sink.
  2. In `tests/integration/saga.rs`: add a 4-step saga test where step 3 compensation fails, asserting that steps 2 and 1 still execute compensation (and that all compensation errors are aggregated). Add a step-panic test verifying compensation trigger.
  3. In `tests/containers/jobs.rs`: add negative tests verifying that `queue.ack(job_id, superseded_worker)` fails when another worker holds the claim.

---

### m1 Dead code and unused import compiler warnings in `containers/pipeline.rs` under non-sqlite feature builds

- **Where:** `rume/tests/containers/pipeline.rs:23, 26, 33, 49, 60, 69`
- **What it says:**
  In `rume/tests/containers.rs:32–34`, `pipeline` is gated behind `#[cfg(feature = "postgres")]`.
  Inside `pipeline.rs`:
  - Lines 49–93 declare `OrderBilledEvent`, `FulfillOrderJob`, and `FulfillOrderHandler` without any feature gate.
  - Lines 96 and 229 gate the tests that use them (`realistic_enterprise_pipeline_end_to_end` and `pipeline_failure_recovery_and_atomicity_invariants`) behind `#[cfg(feature = "sqlite")]`.
  When compiling `cargo check -p rume --tests --no-default-features --features postgres`, the compiler emits 6 warnings:
  ```
  warning: unused imports: `AvailabilityState` and `Availability` (line 23)
  warning: unused import: `authorize` (line 26)
  warning: unused import: `OutboxStatus` (line 33)
  warning: struct `OrderBilledEvent` is never constructed (line 49)
  warning: struct `FulfillOrderJob` is never constructed (line 60)
  warning: struct `FulfillOrderHandler` is never constructed (line 69)
  ```
- **Why it's a problem:**
  Violates clean build standards by generating diagnostic noise during single-feature CI matrix checks.
- **Suggested fix:**
  Gate lines 49–93 in `rume/tests/containers/pipeline.rs` under `#[cfg(feature = "sqlite")]`, or eliminate unused imports and migrate the hybrid SQLite pipeline tests to use the PostgreSQL backend directly.

---

### m2 Redundant duplicate assertions alongside `insta` snapshots in `integration/openapi.rs`

- **Where:** `rume/tests/integration/openapi.rs:30, 88–116`
- **What it says:**
  In `openapi_generation_from_route_registry`, line 30 asserts:
  `insta::assert_json_snapshot!(spec, @r###" ... "###);`
  Directly below, lines 88–116 execute 28 manual `assert_eq!` assertions re-verifying the exact same fields:
  ```rust
  assert_eq!(spec.openapi, "3.1.0");
  assert_eq!(spec.info.title, "User Service");
  assert_eq!(spec.info.version, "1.0.0");
  ...
  assert_eq!(get_op.operation_id.as_deref(), Some("users.show"));
  assert_eq!(get_op.parameters.len(), 1);
  assert_eq!(get_op.parameters[0].name, "id");
  ```
- **Why it's a problem:**
  Manual field assertions duplicate the exact contract enforced by the snapshot, adding boilerplate and maintenance friction without providing additional regression coverage.
- **Suggested fix:**
  Delete lines 88–116 in `openapi.rs` and let the authoritative `insta` snapshot guard the schema.

---

### m3 Inconsistent `#![forbid(unsafe_code)]` vs `#![deny(unsafe_code)]` across crate and test entrypoints

- **Where:** `rume/tests/integration.rs:3` vs `rume/tests/containers.rs:3`, `rume/tests/common/containers.rs:3`, and `rume/src/lib.rs:8`
- **What it says:**
  `rume/src/lib.rs:8`, `rume/tests/containers.rs:3`, and `rume/tests/common/containers.rs:3` enforce `#![forbid(unsafe_code)]`.
  However, `rume/tests/integration.rs:3` enforces `#![deny(unsafe_code)]`.
- **Why it's a problem:**
  `integration.rs` had to downgrade from `forbid` to `deny` because `rume/tests/integration/config.rs:1` uses `#![allow(unsafe_code)]` for its `EnvVarGuard`, which calls `unsafe { std::env::set_var }` / `remove_var` (unsafe in Rust 2024). The difference in policy is undocumented at the entrypoint.
- **Suggested fix:**
  Add a code rationale comment at `rume/tests/integration.rs:3` explaining:
  `// Why: #![deny(unsafe_code)] instead of #![forbid(unsafe_code)] is required to allow integration::config to use unsafe for process environment mutation via EnvVarGuard.`

---

## Positive notes

- **Single-Feature Matrix Compilation Cleanliness**: All documented feature matrix build combinations (`--no-default-features`, `--features sqlite`, `--features postgres`, `--features mysql`, `--features redis`, and `--all-features`) compile cleanly with zero errors, verifying that `rume/tests/common/containers.rs` correctly resolved prior C1.
- **True Zero-Docker Integration Execution**: Eliminating all 15 live Redis containers from `tests/integration/` restored the zero-docker contract. Running `cargo test -p rume --test integration` executes 166 integration tests across routing, OpenAPI, security, validation, sessions, caching, sagas, and metrics in 1.25s with zero Docker daemon access.
- **Resolution of Four Prior Container Failures**: The live container test suite has eliminated the 4 failures that blocked v03/diff/v01 (`postgres_migrations_up_and_rollback`, `worker_panic_does_not_leak_in_flight_count`, `test_worker_heartbeat_and_abandoned_job_recovery`, and `test_redis_idempotency_store`), with all 4 passing cleanly on current code.
- **Potemkin Pool Lifecycle Assertions Eliminated**: `containers/postgres.rs` now verifies `pool.backend().is_closed()` and confirms that post-closure queries fail with `Err(PersistenceError::Closed)`, verifying physical driver shutdown rather than resting on internal boolean flags.
- **Two-Binary Cargo Architecture**: Consolidating 32 flat integration test crates into 2 canonical targets (`integration` and `containers`) continues to save ~9 GB of debug build disk usage and slashes linker invocations by 94%.
- **End-to-End CLI Scaffolding Verification**: `rume-cli/tests/generated_project.rs` tests the complete developer lifecycle: running `rume new`, generating handlers, resources, middleware, models, and services, and verifying that the resulting project compiles and passes tests.

---

## Verdict

**Fail — conditional.**

The test suite and build matrix have made commendable strides: single-feature builds compile across all backends, the zero-docker integration suite is authentic, and all prior broken container tests have been fixed. However, the test suite cannot be certified as production-ready until a fix pass resolves the following **4 Major** findings:

1. **M1** — Eliminate the 33 non-deterministic wall-clock sleeps across `rume/tests/`: replace state-waiting sleeps with `tokio::sync::Notify`, adopt `tokio::time::pause()` for virtual time in in-memory TTL tests, and eliminate multi-second sleeps in `integration/outbox.rs` and `containers/postgres.rs`.
2. **M2** — Make `RumeTestHarness` genuinely usable and adopt it across test suites, decouple it from mandatory outbox repositories, add job runner support, and eliminate the 3 compiler warnings on non-sqlite builds.
3. **M3** — Eliminate or replace the tautological `wiremock` test in `integration/enterprise.rs` with an actual Rume subsystem test.
4. **M4** — Add negative-path tests for event listener panics, saga intermediate compensation failures, and job worker lease fencing.

The three minor polish findings (**m1**–**m3**) regarding pipeline compiler warnings, redundant openapi assertions, and unsafe lint consistency should be resolved in the same fix pass.
