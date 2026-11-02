# Testing Review — v02

## Summary

Rume v02 achieves massive expansion in verification breadth, growing from an initial 19 tests to 154 unit tests, 27 integration test suites, and 25 doc-tests that pass with zero Clippy warnings across strict Rust 2024 compliance. All v01 testing deficiencies (T1–T9) around missing routing tests, untested configuration loading, harness drop hooks, out-of-range state discriminants, and global route registry pollution have been systematically addressed. However, the testing infrastructure and test suite design exhibit four major deficiencies: critical edge-case and failure-mode tests are omitted across newly introduced subsystems (allowing job worker shutdown deadlocks, dead-code timeouts, transport-level circuit breaker bypasses, and unvalidated SAML/multi-tenant boundaries to pass CI undetected); `ArchitectureRules::verify` only inspects direct 1-hop dependencies without checking transitive closures, cycles, or unknown modules; async test suites rely heavily on non-deterministic wall-clock sleeps rather than deterministic time simulation; and `TestApp` / `RumeBuilder::test_app()` bypasses module lifecycle hooks, forcing developers to bind real OS sockets for multi-module integration testing. A targeted fix pass is required before this test suite can serve as an authoritative verification oracle for production.

## Prior decisions

All prior decisions recorded in `docs/reports/reviews/v01/testing.md` (and tracked as `T-T1`–`T-T9` in `docs/reports/reviews/v01/todo.md`) have been audited against the v02 codebase:

- **T1 — `rume/src/routing.rs` has ZERO unit tests** — **REAFFIRMED**. In v02, `routing` was decomposed into dedicated modules, each equipped with comprehensive unit tests: `group.rs` (lines 177–241), `health.rs` (lines 362–660), `matchers.rs` (lines 87–151), `registry.rs` (lines 133–182), `rejection.rs` (lines 45–73), `resource.rs` (lines 126–187), and `validated_param.rs` (lines 89–130). In addition, three dedicated integration suites (`tests/routing.rs`, `tests/health_routes.rs`, and `tests/route_metadata.rs`) verify HTTP parameter validation, route merging, and metadata introspection end-to-end.
- **T2 — No fails-before proof anywhere in the suite** — **REAFFIRMED**. Commit history confirms that v01 bug fixes and their accompanying regression tests were merged in single atomic commits without preserving failing test artifacts or `todo_*` reproduction harnesses. While rigorous regression assertions were added for v01 findings, the underlying failure mode recurred in v02: several newly introduced test suites exercise only happy paths and benign inputs, passing cleanly while severe failure modes and dead code remained undetected (evaluated in finding M1).
- **T3 — `rume/src/config.rs` is entirely untested** — **REAFFIRMED**. In v02, `rume/src/config.rs` lines 104–182 adds unit tests covering `load_raw_not_found_when_no_config`, `load_raw_reads_app_toml`, `load_raw_pretty_returns_valid_toml`, and `load_raw_local_toml_is_loaded`. (A minor defect regarding fixed `/tmp` paths is recorded in finding m3).
- **T4 — `TestApp`/`TestAppBuilder` test harness is itself untested** — **REAFFIRMED**. In v02, `rume/src/testing.rs` lines 151–268 introduces 7 unit tests explicitly labeled `T-T4`, verifying `TestApp::new`, `post_json`, `TestAppBuilder::http`, custom state binding, cleanup hooks on `Drop`, explicit `teardown`, and the `status` helper.
- **T5 — `app.rs` tests miss boundary, Inspect-mode, and lifecycle-ordering surface** — **REAFFIRMED**. In v02, `rume/src/app/state.rs` lines 205–234 tests that invalid `u8` values map deterministically to `Created` and `Unknown`; `rume/src/app/builder.rs` lines 1381–1440 tests that `ApplicationMode::Inspect` never binds listeners or executes ready/shutdown hooks; and `rume/tests/lifecycle_shutdown.rs` verifies lifecycle and availability transitions across server shutdown.
- **T6 — `error.rs` tests cover 3 of 6 `AppError` variants and assert no response bodies** — **REAFFIRMED**. In v02, `rume/src/error.rs` lines 286–455 tests all variants (`NotFound`, `Unauthorized`, `Forbidden`, `BadRequest`, `Internal`, `Validation`, and `Public`), asserts RFC 9457 Problem Details JSON bodies, and verifies that internal diagnostics are never leaked to HTTP clients.
- **T7 — `env.rs` has no `file:` traversal test, and integration env tests mutate global process env** — **REAFFIRMED in part, CHALLENGED in part**. Path traversal rejection is now thoroughly tested by `file_value_traversal_is_rejected` and `file_value_absolute_path_is_rejected` in `rume/src/env/loading.rs:300–319`. However, the challenge remains for hermeticity: `rume/tests/env_and_config_loading.rs` still mutates the global process environment with `unsafe { std::env::set_var }` without restoring original environment values on test completion (tracked in finding m2).
- **T8 — `http_smoke.rs` tests a bare router, not the real health surface** — **REAFFIRMED**. While `http_smoke.rs` was retained as a minimal sanity check, a dedicated integration suite `rume/tests/health_routes.rs` was added to thoroughly test `health_routes()` with `AppContext` state, `HealthChecks::run`, degraded `Warning` (503), failed checks (503), and empty `debug_info`.
- **T9 — No test isolates or cleans up `ROUTE_REGISTRY` global state across tests** — **REAFFIRMED**. The root cause was eliminated: the process-global `ROUTE_REGISTRY` singleton was abolished in favor of an application-scoped `RouteRegistry` struct embedded in `AppContext` and `Router` (`rume/src/routing/registry.rs:39–50`), preventing parallel test pollution.
- **NOTE-NEW — `ModuleHarness` isolated module test harness (`rume/src/module/harness.rs`)** — Introduced in v02; evaluated in findings M4 and m1.
- **NOTE-NEW — `ArchitectureRules` CI enforcement (`rume/src/module/architecture.rs`)** — Introduced in v02; evaluated in finding M2.
- **NOTE-NEW — Subsystem integration test suites (`rume/tests/`)** — 18 new integration suites covering auth, cache, coordination, enterprise, events, jobs, metrics, outbox, persistence, resilience, routing, sagas, security, and multi-tenancy; evaluated in findings M1 and M3.

---

## Findings

### M1 Happy-path test bias across newly introduced subsystems fails to verify critical failure modes, error paths, and security boundaries

- **Where:** `rume/tests/jobs_suite.rs`, `rume/tests/resilience_suite.rs`, `rume/tests/persistence_integration.rs`, `rume/tests/enterprise_suite.rs`, `rume/tests/tenant_suite.rs`, and `rume/tests/auth_suite.rs`.
- **What it says:**
  The integration suites for newly introduced v02 subsystems almost exclusively verify successful executions and expected HTTP status codes, omitting negative test cases, error propagation, panics, timeouts, and boundary collisions:
  1. *Background Jobs (`jobs_suite.rs`)*: Lines 228–251 test `worker.drain` waiting for `SlowJob`. There are **zero tests** verifying worker behavior when a job handler panics, and **zero tests** verifying job timeouts (`Job::timeout()`). This blindspot allowed Correctness C1 (handler panic permanently leaks `in_flight_count` and hangs `drain()`) and Correctness M2 (`Job::timeout()` is dead code) to pass CI completely unnoticed.
  2. *Resilience (`resilience_suite.rs`)*: Lines 10–71 test `CircuitBreakerLayer` by returning HTTP 500 status codes. There are **zero tests** testing downstream Tower / transport-level `Err` results (e.g. broken connection, timeout, channel exhaustion). This blindspot directly masked Correctness M1 (`CircuitBreakerService` bypasses failure accounting on `Err` responses).
  3. *Persistence (`persistence_integration.rs`)*: Lines 103–149 test migration rollbacks only where `down_sql` is explicitly defined via `.with_down(...)`. There are **zero tests** for rolling back irreversible migrations where `down_sql` is `None`. This allowed Correctness M3 (`Migrator::rollback_last` silently unmarking migrations without executing SQL) to pass CI.
  4. *Enterprise SAML (`enterprise_suite.rs`)*: Lines 32–82 test SAML parsing exclusively with valid, unexpired XML and matching audiences. There are **zero tests** for expired assertions (`NotOnOrAfter` in the past), validity window enforcement (`NotBefore` in the future), XML comment injection, or missing required attributes, masking Security C1.
  5. *Multi-Tenant Boundaries (`tenant_suite.rs`)*: Lines 76–98 test `SubdomainTenantResolver` only with benign hosts (`globex.app.platform.io`), and lines 139–191 test `TenantScopedCache` only with colon-free tenant IDs (`"tenant-a"`, `"tenant-b"`). There are **zero tests** for rogue subdomain suffix spoofing (`evilapp.platform.io`, Security M1) and **zero tests** for delimiter injection (`tenant:t1:user:42`, Security C2).
  6. *Authentication Schemes (`auth_suite.rs`)*: Lines 181–207 test `OptionalAuth` with `ApiKeyScheme`, but **zero tests** verify `OptionalAuth` with `SessionScheme` when an expired session cookie is present. This allowed Security m2 (stale session cookie aborts entire auth pipeline) to escape detection.
- **Why it's a problem:** A test suite that only verifies happy paths creates a dangerous false sense of security. Passing `cargo test` 100% does not prove system reliability if the failure modes and security boundaries that production systems rely on are never exercised.
- **Suggested fix:** Expand the integration suites to mandate negative and adversarial test cases for every subsystem:
  - Add `job_handler_panic_does_not_leak_in_flight_counter_or_block_drain` to `jobs_suite.rs`.
  - Add `job_execution_aborts_on_timeout` asserting `JobError::Timeout` to `jobs_suite.rs`.
  - Add `circuit_breaker_trips_on_service_transport_errors` to `resilience_suite.rs`.
  - Add `rollback_last_fails_when_down_sql_is_none` to `persistence_integration.rs`.
  - Add `saml_assertion_expired_returns_error` and `saml_comment_injection_is_rejected` to `enterprise_suite.rs`.
  - Add `tenant_cache_delimiter_isolation` and `subdomain_spoofing_rejected` to `tenant_suite.rs`.
  - Add `optional_auth_succeeds_as_guest_when_session_cookie_expired` to `auth_suite.rs`.

---

### M2 `ArchitectureRules::verify` only inspects direct dependencies and ignores transitive violations, undeclared modules, and cycles

- **Where:** `rume/src/module/architecture.rs:60–93` and `rume/tests/module_testing_and_architecture_suite.rs:91–133`.
- **What it says:**
  ```rust
  // 2. Verify forbidden dependencies
  for module in modules {
      let from_name = module.name();
      let declared_deps = module.dependencies();

      for (forbidden_from, forbidden_to) in &self.forbidden_deps {
          if from_name == forbidden_from && declared_deps.contains(&forbidden_to.as_str()) {
              return Err(ArchitectureViolation {
                  rule: "forbid_dependency".to_string(),
                  details: format!(
                      "module '{from_name}' is forbidden from depending on '{forbidden_to}', but declares it as a dependency"
                  ),
              });
          }
      }
  }
  ```
- **Why it's a problem:**
  1. *Transitive bypass*: `verify` only checks the immediate 1-hop `declared_deps` slice. If an architecture rule specifies `forbid_dependency("billing", "database_internal")`, and module `"billing"` depends on `"orders"`, which in turn depends on `"database_internal"`, `declared_deps` for `"billing"` is `["orders"]`. The forbidden transitive dependency (`billing -> orders -> database_internal`) is completely ignored by `verify`, producing a false-clean pass in CI.
  2. *Undeclared module dependencies*: If a module declares a dependency on a module that is not registered in `modules` (e.g. `dependencies() -> vec!["nonexistent"]`), `ArchitectureRules::verify` ignores it and returns `Ok(())`.
  3. *Cycle blindness*: If modules contain circular dependencies (`A -> B -> A`), `ArchitectureRules::verify` returns `Ok(())` without detecting the cycle.
  4. *Tautological test coverage*: In `module_testing_and_architecture_suite.rs`, the test `architecture_rules_enforce_forbidden_cross_domain_calls` only tests direct 1-hop dependencies (`billing -> frontend`), asserting that `verify` works without ever testing transitive violations, cycles, or missing modules.
- **Suggested fix:**
  1. In `ArchitectureRules::verify`, compute the transitive dependency closure for each module (or perform a reachability traversal using `ModuleRegistry`'s DFS algorithm) to reject forbidden dependencies reached directly or indirectly.
  2. Verify that every module name in `declared_deps` exists in the registered `modules` list.
  3. Run cycle detection across `modules` during `verify` and return an `ArchitectureViolation` if cycles are present.
  4. Add integration tests in `module_testing_and_architecture_suite.rs` for transitive forbidden dependencies (`A -> B -> C`), missing dependencies, and cycles.

---

### M3 Widespread non-deterministic wall-clock sleeps across async tests create flaky test risks and slow execution

- **Where:** Across 9 test suites in `rume/tests/`:
  - `lifecycle_shutdown.rs`: lines 103, 122, 172, 180, 194 (`sleep(Duration::from_millis(30..250))`)
  - `jobs_suite.rs`: lines 80, 131, 189, 220, 244 (`sleep(Duration::from_millis(20..80))`)
  - `job_reliability_suite.rs`: line 79 (`sleep(Duration::from_millis(30))`)
  - `resilience_suite.rs`: lines 58, 135, 150, 173 (`sleep(Duration::from_millis(15..80))`)
  - `cache_suite.rs`: line 71 (`sleep(Duration::from_millis(30))`)
  - `coordination_suite.rs`: line 51 (`sleep(Duration::from_millis(30))`)
  - `events_suite.rs`: lines 107, 124 (`sleep(Duration::from_millis(20..50))`)
  - `progressive_composition.rs`: lines 224, 377 (`sleep(Duration::from_millis(50))`)
  - `request_identity.rs`: line 324 (`sleep(Duration::from_millis(5))`)
- **What it says:**
  More than 20 async tests use raw `tokio::time::sleep(Duration::from_millis(N)).await` to wait for background tasks, rate limiting windows, server state transitions, and lock TTL expirations. For example, in `resilience_suite.rs:149–153`:
  ```rust
  let h1 = tokio::spawn(async move { app1.get("/compute").await });
  let h2 = tokio::spawn(async move { app2.get("/compute").await });

  // Brief sleep to let both acquire permits
  sleep(Duration::from_millis(15)).await;

  // 3rd concurrent request exceeds limit of 2 -> 503 SHED_LOAD
  let res3 = app.get("/compute").await;
  ```
  and in `lifecycle_shutdown.rs:102–104`:
  ```rust
  // Give server a moment to start and declare Ready
  tokio::time::sleep(Duration::from_millis(50)).await;
  assert_eq!(server_ctx.lifecycle.state(), LifecycleState::Ready);
  ```
- **Why it's a problem:**
  1. *Flaky race conditions*: A 15ms or 50ms sleep assumes the OS thread scheduler will dispatch the spawned Tokio tasks before the sleep finishes. Under heavy CPU load in CI environments (e.g. parallel test runners on shared runners), 15ms is frequently insufficient. If `h1` or `h2` has not acquired its concurrency permit before `res3` executes, `res3` gets permit #2 and returns 200 OK instead of 503, causing spurious test failure.
  2. *Slow CI execution*: Accumulating arbitrary sleeps across 27 integration suites adds several seconds of idle latency to every test run.
  3. *Absence of deterministic simulation*: Across the entire codebase, **zero tests** utilize Tokio's built-in time pausing capabilities (`tokio::time::pause()`, `tokio::time::advance()`), which allows instant, 100% deterministic testing of timeouts, TTLs, and sliding windows without burning real time or risking scheduler races.
- **Suggested fix:**
  1. In concurrency and shutdown tests, replace sleeps with explicit synchronization primitives (e.g. `tokio::sync::Barrier`, `tokio::sync::oneshot`, or `watch` channels) so tests progress immediately when background tasks reach the desired state.
  2. In TTL, rate-limiting, and timeout tests (`cache_suite.rs`, `coordination_suite.rs`, `resilience_suite.rs`, `job_reliability_suite.rs`), use `tokio::time::pause()` and `tokio::time::advance(duration).await` for instantaneous, deterministic simulation.

---

### M4 `TestApp` and `RumeBuilder::test_app()` bypass module lifecycle hooks, forcing full TCP socket binds for multi-module integration testing

- **Where:** `rume/src/app/builder.rs:536–543`, `rume/src/testing.rs:8–86`, and `rume/tests/progressive_composition.rs:215–225, 365–385`.
- **What it says:**
  In `rume/src/app/builder.rs`:
  ```rust
  pub fn test_app(self) -> Result<crate::testing::TestApp, std::io::Error> {
      let ctx = self.ctx.clone();
      ctx.lifecycle.set(crate::app::state::LifecycleState::Ready);
      ctx.availability
          .set(crate::app::state::AvailabilityState::Ready);
      let app_router = self.build_router(ctx.clone())?;
      Ok(crate::testing::TestApp::new(app_router.with_state(ctx)))
  }
  ```
- **Why it's a problem:**
  `RumeBuilder::test_app()` is synchronous and merely sets `LifecycleState::Ready` and `AvailabilityState::Ready` manually. It **never executes module `on_start` lifecycle hooks** or application `on_ready` hooks. If an application is composed of modules that initialize database schemas, pre-warm caches, or spawn background workers during `on_start`, calling `builder.test_app()` yields an uninitialized application.
  Because `TestApp` only wraps an in-memory `Router<()>` and has no lifecycle awareness (startup, rollback, shutdown, drain), developers who need to test multi-module application lifecycles cannot use `TestApp`. As seen in `progressive_composition.rs` and `lifecycle_shutdown.rs`, tests are forced to bind real OS network sockets (`TcpListener::bind("127.0.0.1:0")`), spawn background server threads, and send raw HTTP bytes over `TcpStream`. This defeats the purpose of having a fast, lightweight, hermetic in-memory test harness.
- **Suggested fix:**
  Provide an asynchronous `pub async fn build_test_app(self) -> Result<TestApp, BuildError>` on `RumeBuilder` that:
  1. Resolves module startup order and executes all module `on_start` hooks in topological order with failure rollback.
  2. Executes application `on_ready` hooks.
  3. Attaches an explicit teardown hook to `TestApp` that executes module `on_shutdown` hooks in reverse topological order upon `app.teardown()` or `Drop`.

---

### m1 `ModuleHarness` lacks health check verification and error-path test coverage

- **Where:** `rume/src/module/harness.rs:70–75` and `rume/tests/module_testing_and_architecture_suite.rs:41–67`.
- **What it says:**
  `ModuleHarness<M>` provides `context()`, `module()`, `start()`, `shutdown()`, and `test_app()`. However, it provides no helper to inspect or execute the health checks registered by the module (`module.register_health_checks(&mut checks)`). Furthermore, `module_testing_and_architecture_suite.rs` tests only the happy path of `ModuleHarness`: `start()` failing with `ModuleError::StartupFailed`, calling `shutdown()` when `start()` was never called, or repeated shutdowns are completely untested.
- **Why it's a problem:** Developers using `ModuleHarness` to test domain modules in isolation have no ergonomic way to verify that their module's diagnostic health checks function correctly. Gaps in harness error handling remain unverified.
- **Suggested fix:**
  1. Add `pub fn health_checks(&self) -> HealthChecks` and `pub async fn run_health_checks(&self) -> Vec<HealthCheckResult>` to `ModuleHarness`.
  2. Add unit/integration tests asserting that `ModuleHarness::start` returns `Err(ModuleError::StartupFailed { .. })` when `on_start` fails, and that repeated `shutdown()` calls are idempotent.

---

### m2 Process environment mutation in `env_and_config_loading.rs` is not hermetically restored

- **Where:** `rume/tests/env_and_config_loading.rs:8–20, 26–153`.
- **What it says:**
  ```rust
  static ENV_LOCK: Mutex<()> = Mutex::new(());

  fn set_env(key: &str, value: &str) {
      unsafe { std::env::set_var(key, value); }
  }
  ```
  In tests `loads_layered_env_files`, `skips_local_files_in_test_env`, `loads_merged_config`, and `loads_typed_env`, `set_env("RUME_ENV", ...)` is called. At the end of these tests, `RUME_ENV` is never unset.
- **Why it's a problem:** Mutating process-wide environment variables in multi-threaded test environments leaks state across the test binary. If any subsequent test or test suite checks `std::env::var("RUME_ENV")`, it observes `"development"` instead of the clean process default, violating test hermeticity.
- **Suggested fix:** Implement an RAII `EnvVarGuard` that records original values and automatically restores them upon `Drop`, guaranteeing that all modified environment variables are cleaned up even if an assertion panics.

---

### m3 Config unit tests use hardcoded `/tmp` paths rather than isolated `tempfile::tempdir()`

- **Where:** `rume/src/config.rs:114–181`.
- **What it says:**
  ```rust
  let dir = std::env::temp_dir().join("rume_config_test_not_found");
  let _ = fs::remove_dir_all(&dir);
  fs::create_dir_all(&dir).ok();
  ```
  Tests in `config.rs` hardcode directory paths in the system temp directory (`rume_config_test_not_found`, `rume_config_test_app_toml`, `rume_config_test_pretty`, `rume_config_test_local`).
- **Why it's a problem:** If two test runners or CI jobs execute concurrently on the same machine, or if a previous test run crashed leaving directory locks or altered permissions, the tests collide on these fixed filesystem locations.
- **Suggested fix:** Replace hardcoded path joins with `tempfile::tempdir()`, which generates unique, cryptographically random temporary directories that automatically clean up on drop.

---

## Positive notes

- **Comprehensive v01 resolution:** All 9 testing findings from v01 (`T1`–`T9`) were directly resolved. The addition of dedicated unit test suites for `routing/` (matchers, resources, rejections, registry, groups, health), `testing.rs` harness self-tests, and `config.rs` tests represents a major improvement in codebase rigor.
- **Application-scoped route registry isolation:** Completely replacing the process-global `ROUTE_REGISTRY` with application-scoped `RouteRegistry` on `AppContext` and `Router` eliminated cross-test mutex contention and state leakage across parallel test runs.
- **Multi-extractor integration depth in `validation_suite.rs`:** Tests in `validation_suite.rs` comprehensively exercise compound extractors (`ValidatedPath`, `ValidatedQuery`, `ValidatedJson`, `ValidatedForm`), verifying both successful multi-parameter extraction and RFC 9457 Problem Details error formatting with structured field details.
- **Reverse compensation verification in `saga_suite.rs`:** `saga_suite.rs` explicitly tests forward step progression, strict reverse chronological compensation upon failure, and short-circuit error handling when compensation steps fail.
- **Clean in-memory request dispatch in `TestApp`:** `TestApp::request` invokes `router.clone().oneshot(req)` in memory without opening network ports, providing fast, deterministic HTTP testing for stateless routers.
- **CSPRNG and tracing span isolation in `request_identity.rs`:** 10 integration tests verify UUID generation, header preservation, sanitization of unsafe input, and preservation of tracing spans across asynchronous `.await` yields.
- **Strict Clippy and formatting cleanliness:** The entire suite of 154 unit tests, 27 integration tests, and 25 doc-tests passes with 0 warnings under `#![forbid(unsafe_code)]` and strict Rust 2024 compliance.

---

## Verdict

**Fail — conditional.**

The test suite has expanded dramatically in volume and cleanliness since v01, resolving all prior findings. However, it cannot be certified as an authoritative verification oracle ready for production release until the following **4 Major** findings are resolved:

1. **M1** — Eliminate happy-path bias across newly introduced subsystems by adding tests for critical failure modes: job handler panics, job timeouts, Tower service transport errors in circuit breaking, rollback of irreversible migrations, expired/malformed SAML assertions, and multi-tenant delimiter collisions.
2. **M2** — Enhance `ArchitectureRules::verify` to inspect transitive dependency closures, detect dependency cycles, and verify that all declared dependencies exist in the module registry.
3. **M3** — Replace non-deterministic wall-clock sleeps across async tests with explicit synchronization primitives (barriers, channels) and Tokio deterministic time simulation (`tokio::time::pause()` / `advance()`).
4. **M4** — Provide an asynchronous `RumeBuilder::build_test_app()` harness that executes module lifecycle hooks (`on_start`/`on_shutdown`) and rollback in memory, eliminating the need to bind raw OS network sockets for multi-module integration testing.

The minor findings (`m1`–`m3`) regarding `ModuleHarness` health check inspection, process environment restoration, and config temporary directories should be resolved in the same fix pass.
