# Correctness Review — v02

## Summary

The Rume v02 codebase demonstrates substantial architectural maturation across its runtime contracts: the v01 blockers around `ValidatedParam` compilation, `Inspect` mode HTTP exposure, `u8` discriminant safety, and route separator joining have all been cleanly resolved. However, critical and major correctness gaps remain in background worker reliability and resilience middleware: a panic in a job handler permanently leaks the in-flight job counter and locks graceful shutdown drain, `CircuitBreakerService` bypasses failure accounting on transport-level errors, `Job::timeout()` is dead code that leaves workers vulnerable to indefinite hangs, and `Migrator::rollback_last` silently corrupts migration state on irreversible steps. The implementation is not yet ready for production deployment until these lifecycle and error handling defects are addressed.

## Prior decisions

All prior decisions recorded in `docs/reports/reviews/v01/correctness.md` (and summarized in `docs/reports/reviews/v01/todo.md`) have been audited against the current implementation:

- **C1 — `ValidatedParam<M>` cannot compile with `AppContext` state** — **REAFFIRMED**. The v01 finding was that `impl FromRequestParts<()>` hardcoded state to `()`, breaking handlers mounted on `Router<AppContext<E, C, S>>`. In v02, this was fully resolved by moving HTTP-time parameter validation to `ParamValidationLayer` on `RouteSpec` (`rume/src/routing/spec.rs:166-302`), which inspects path parameters before handler invocation and rejects invalid inputs with 400 Problem Details. `ValidatedParam` was decoupled into a standalone validation helper (`ValidatedParam::from_value`), and generic typed extraction is now provided by `ValidatedPath<T>` in `rume/src/validation.rs:156-200`. Handlers compile cleanly with any application state.
- **C2 — `ApplicationMode::Inspect` still serves HTTP traffic** — **REAFFIRMED**. The v01 finding was that `run()` called `axum::serve` unconditionally. In v02, `rume/src/app/builder.rs:909-932` and `rume/src/app/builder.rs:993-1014` explicitly guard execution: when `mode == ApplicationMode::Inspect`, the context transitions to `LifecycleState::Booted` and `AvailabilityState::Live`, outputs route metadata when requested, and immediately returns `Ok(())` without binding a listener or serving HTTP traffic. Verified by unit tests `inspect_mode_sets_booted_live_then_returns`, `inspect_mode_does_not_run_on_ready_hooks`, and `inspect_mode_does_not_run_on_shutdown_hooks`.
- **C3 — `Lifecycle::set` / `Availability::set` accept any `u8`** — **REAFFIRMED**. The v01 finding was that unvalidated `u8` stores masked state corruption. In v02, `rume/src/app/state.rs:47-54, 114-121` adds `#[repr(u8)]` to `LifecycleState` and `AvailabilityState`, defines `MAX_DISCRIMINANT = 4`, and enforces `if value <= Self::MAX_DISCRIMINANT` guards in `set()`. Out-of-range memory reads map deterministically to `Created` and `Unknown` without panicking. Verified by unit tests `lifecycle_invalid_u8_maps_to_created` and `availability_invalid_u8_maps_to_unknown`.
- **C4 — Clarification on v01 report finding taxonomy** — **REAFFIRMED**. The original `v01/correctness.md` report recorded three critical/major findings (`C1`, `C2`, `C3`) and four minor findings (`m1`–`m4`). The prompt's reference to `C1`–`C4` is reaffirmed as encompassing the complete v01 finding set; no distinct fourth critical finding existed in v01 to challenge or overturn.
- **m1 — `Matcher::Number` doc ("digits only") does not match `u64`-bounded parsing** — **REAFFIRMED**. In v02, `rume/src/routing/matchers.rs:15-16, 22-25` explicitly documents that values must parse as `u64` and returns generic error messages without echoing user input. The documentation and behavior are fully reconciled.
- **m2 — `RouteGroup::add` does not normalize path separators** — **REAFFIRMED**. In v02, `join_path` was introduced in `rume/src/routing/registry.rs:109-130` and integrated across all `RouteGroup` mounting methods (`rume/src/routing/group.rs:42, 53, 64, 75, 86, 93, 104, 126, 146`), trimming adjacent slashes and preventing double-slash route mismatches.
- **m3 — `RouteSpec::build()` registers in global registry even when unmounted** — **REAFFIRMED**. In v02, process-global `ROUTE_REGISTRY` was eliminated. `RouteRegistry` is now application-scoped on `AppContext` and `Router`. `RouteSpec::build()` returns metadata without registration side effects (`rume/src/routing/spec.rs:153-164`), and registration occurs only when routes are explicitly mounted.
- **m4 — `health_routes` `/health/ready` returns 503 for `Warning`, not just failures** — **REAFFIRMED**. The v01 report noted a discrepancy between code and doc wording. In v02, the architectural decision was affirmed that readiness requires complete health (`is_healthy: false` on `Warning`), and documentation and tests (`rume/src/routing/health.rs:140-144, 243-248, 618-626`) were updated to explicitly document that `Warning` is degraded and returns 503.

---

## Findings

### C1 Panic during job execution permanently leaks `in_flight_count` and hangs graceful shutdown `drain()`

- **Where:** `rume/src/jobs/worker.rs:133-141` and `rume/src/jobs/worker.rs:176-188`
- **What it says:**
  ```rust
  self.in_flight_count.fetch_add(1, Ordering::SeqCst);
  let queue = Arc::clone(&self.queue);
  let registry = Arc::clone(&self.registry);
  let in_flight = Arc::clone(&self.in_flight_count);

  tokio::spawn(async move {
      Self::execute_job(queue, registry, job).await;
      in_flight.fetch_sub(1, Ordering::SeqCst);
  });
  ```
  and `drain()`:
  ```rust
  pub async fn drain(&self, timeout: Duration) -> bool {
      self.is_stopping.store(true, Ordering::SeqCst);
      let start = std::time::Instant::now();

      while self.in_flight_count.load(Ordering::SeqCst) > 0 {
          if start.elapsed() >= timeout {
              return false;
          }
          sleep(Duration::from_millis(20)).await;
      }

      true
  }
  ```
- **Why it's a problem:** In `tokio::spawn`, `in_flight.fetch_sub(1, Ordering::SeqCst)` is placed immediately after `Self::execute_job(...).await` without an RAII drop guard or panic boundary. If a registered job handler panics during execution, the spawned task terminates immediately during panic unwinding. `in_flight.fetch_sub` never executes. As a consequence:
  1. `in_flight_count` is permanently leaked by +1 for each panicked job.
  2. Any subsequent invocation of `JobWorker::drain(timeout)` will observe `in_flight_count > 0` forever, hang until the timeout expires, and return `false`, causing application shutdown hooks to fail or time out.
  3. Furthermore, because `execute_job` was aborted mid-flight, the panicked job is neither acknowledged, re-queued (`nack`), nor moved to the dead-letter queue; in `MemoryJobQueue`, it remains leaked in `state.in_flight` indefinitely.
- **Suggested fix:**
  1. Wrap the decrement in an RAII guard struct whose `Drop` implementation executes `in_flight.fetch_sub(1, Ordering::SeqCst)` unconditionally upon task exit.
  2. In `execute_job`, wrap handler invocation in `std::panic::AssertUnwindSafe(...).catch_unwind()` to catch panics and route the failed job to `queue.dead_letter(job, "handler panicked")` rather than abandoning it.

---

### M1 `CircuitBreakerService` skips failure recording on downstream Tower / network `Err` responses

- **Where:** `rume/src/resilience/circuit_breaker.rs:189-209`
- **What it says:**
  ```rust
  fn call(&mut self, req: Request<Body>) -> Self::Future {
      let breaker = self.breaker.clone();
      let mut inner = self.inner.clone();

      Box::pin(async move {
          if !breaker.allow_request() {
              let err: AppError = ResilienceError::CircuitOpen.into();
              return Ok(err.into_response());
          }

          let response = inner.call(req).await?;

          if response.status().is_server_error() {
              breaker.on_failure();
          } else {
              breaker.on_success();
          }

          Ok(response)
      })
  }
  ```
- **Why it's a problem:** Line 199 uses the `?` operator on `inner.call(req).await`. In Tower services and HTTP clients, infrastructure and transport failures (e.g. broken TCP connections, connection refused, connection reset, upstream timeouts, channel exhaustion) return `Err(err)`. The `?` operator immediately returns from the async block with `Err(err)`, bypassing lines 201–205 entirely. As a result, `breaker.on_failure()` is **never called** when the downstream service fails with a transport error; it is only called when downstream responds with an HTTP response object whose status code is 5xx. During an outage where a service is unreachable or crashing, the circuit breaker remains in `CircuitState::Closed` and never trips to `CircuitState::Open`, defeating the primary purpose of circuit breaking.
- **Suggested fix:** Match on the `Result` of `inner.call(req).await` before applying `?`:
  ```rust
  let response = match inner.call(req).await {
      Ok(resp) => {
          if resp.status().is_server_error() {
              breaker.on_failure();
          } else {
              breaker.on_success();
          }
          resp
      }
      Err(err) => {
          breaker.on_failure();
          return Err(err);
      }
  };
  Ok(response)
  ```

---

### M2 `Job::timeout()` is never enforced during job execution, leaving `JobError::Timeout` as dead code

- **Where:** `rume/src/jobs/job.rs:28-30` and `rume/src/jobs/worker.rs:63-75, 154-173`
- **What it says:**
  In `rume/src/jobs/job.rs`:
  ```rust
  /// Execution timeout for a single attempt.
  fn timeout(&self) -> Option<Duration> {
      Some(Duration::from_secs(60))
  }
  ```
  In `rume/src/jobs/error.rs`:
  ```rust
  /// The job exceeded its execution timeout.
  #[error("job execution timed out")]
  Timeout,
  ```
  In `rume/src/jobs/worker.rs`:
  ```rust
  async fn execute_job(queue: Arc<Q>, registry: Arc<JobRegistry>, job: QueuedJob) {
      let job_id = job.id.clone();
      match registry.dispatch(&job).await {
  ```
- **Why it's a problem:** The `Job` trait documents and specifies a per-job execution timeout (`timeout(&self) -> Option<Duration>`), defaulting to 60 seconds, and `JobError::Timeout` exists in the error enum. However:
  1. `QueuedJob` (`rume/src/jobs/job.rs:52-65`) does not store the timeout duration.
  2. `JobQueue::enqueue_delayed` (`rume/src/jobs/queue.rs:77-111`) never calls or stores `job.timeout()`.
  3. `JobRegistry::dispatch` and `JobWorker::execute_job` await handler execution directly with no `tokio::time::timeout` wrapper.
  4. Across the entire repository, `JobError::Timeout` is unreferenced dead code.
  If a background job hangs (e.g. blocked socket, deadlocked mutex, infinite loop), it will execute forever, consuming a worker task and permanently blocking `JobWorker::drain(timeout)` during graceful shutdown.
- **Suggested fix:** Add `timeout: Option<Duration>` to `QueuedJob`, populate it in `enqueue_delayed` from `job.timeout()`, and in `JobWorker::execute_job`, wrap `registry.dispatch(&job)` in `tokio::time::timeout`:
  ```rust
  let dispatch_fut = registry.dispatch(&job);
  let result = match job.timeout {
      Some(dur) => match tokio::time::timeout(dur, dispatch_fut).await {
          Ok(res) => res,
          Err(_) => Err(JobError::Timeout),
      },
      None => dispatch_fut.await,
  };
  ```

---

### M3 `Migrator::rollback_last` silently unmarks migrations when `down_sql` is `None`

- **Where:** `rume/src/persistence/migration.rs:132-145`
- **What it says:**
  ```rust
  for migration in self.migrations.iter().rev() {
      if db.has_migration(&migration.id) {
          if let Some(sql) = &migration.down_sql {
              db.execute(sql)
                  .await
                  .map_err(|err| PersistenceError::MigrationError {
                      migration: migration.id.clone(),
                      message: err.to_string(),
                  })?;
          }
          db.unmark_migration(&migration.id);
          return Ok(Some(migration.id.clone()));
      }
  }
  ```
- **Why it's a problem:** `Migration::from_sql` constructs migrations with `down_sql: None` by default (`rume/src/persistence/migration.rs:50`). If an irreversible migration or a migration created without a rollback script is rolled back via `migrator.rollback_last(db)`, the method skips SQL execution entirely, calls `db.unmark_migration(&migration.id)`, and returns `Ok(Some(id))`. The database schema changes remain in the database, but the migrator now records the migration as unapplied. A subsequent call to `run_pending` will attempt to re-execute the migration's `up_sql`, triggering SQL table/column already exists errors or corrupted data.
- **Suggested fix:** If `migration.down_sql.is_none()`, fail closed by returning an error instead of unmarking:
  ```rust
  if db.has_migration(&migration.id) {
      let Some(sql) = &migration.down_sql else {
          return Err(PersistenceError::MigrationError {
              migration: migration.id.clone(),
              message: "cannot roll back migration: no down_sql provided".to_string(),
          });
      };
      db.execute(sql).await.map_err(|err| PersistenceError::MigrationError {
          migration: migration.id.clone(),
          message: err.to_string(),
      })?;
      db.unmark_migration(&migration.id);
      return Ok(Some(migration.id.clone()));
  }
  ```

---

### m1 `ValidatedParam` documentation refers to "extractor", but `FromRequestParts` is no longer implemented

- **Where:** `rume/src/routing/validated_param.rs:1-3, 10-15, 61-64`
- **What it says:**
  ```rust
  //! Validated parameter extraction.
  //!
  //! Provides [`ValidatedParam`], which validates a route parameter value
  //! against a [`Matcher`] and returns the validated string.
  ...
  /// Consume the extractor and return the validated string value.
  pub fn into_inner(self) -> String {
  ```
- **Why it's a problem:** After resolving v01 finding C1, `ValidatedParam<M>` no longer implements Axum's `FromRequestParts` trait. Instead, route parameter validation is handled by `RouteSpec::validation_layer` (`ParamValidationLayer`) or `ValidatedPath<T>` (`rume/src/validation.rs:156-200`). Calling `ValidatedParam` an "extractor" and naming the module "Validated parameter extraction" is stale documentation that misleads developers into attempting to extract `ValidatedParam<M>` in handler parameters (`fn handler(id: ValidatedParam<Number>)`), which fails compilation.
- **Suggested fix:** Clarify in the module doc and `into_inner` docstring that `ValidatedParam` is a validated value container populated via `ValidatedParam::from_value`, and direct developers to `RouteSpec::where_param` / `ParamValidationLayer` or `ValidatedPath` for Axum handler extraction.

---

### m2 `CircuitBreaker` permits unbounded concurrency during `HalfOpen` recovery probe

- **Where:** `rume/src/resilience/circuit_breaker.rs:96-101`
- **What it says:**
  ```rust
  pub fn allow_request(&self) -> bool {
      match self.state() {
          CircuitState::Closed | CircuitState::HalfOpen => true,
          CircuitState::Open => false,
      }
  }
  ```
- **Why it's a problem:** When `reset_timeout` expires, `state()` transitions from `Open` to `HalfOpen`. In `HalfOpen`, `allow_request()` returns `true` for all incoming requests without concurrency limits. Under high load, hundreds of concurrent requests will immediately flood the recovering service simultaneously, rather than allowing a small, controlled trial probe (e.g. bounded by `success_threshold` = 2). This causes an immediate thundering herd on downstream services that are just starting to recover.
- **Suggested fix:** Limit the number of concurrent in-flight requests permitted while in `CircuitState::HalfOpen` to `config.success_threshold`, shedding excess requests with `ResilienceError::CircuitOpen` until the probes either succeed (closing the circuit) or fail (reopening it).

---

## Positive notes

- **RFC 9457 Problem Details consistency:** `rume/src/error.rs` is exceptionally disciplined. All error responses use `application/problem+json`, emit the standard fields (`type`, `title`, `status`, `detail`, `instance`, `code`, `request_id`), log raw internal diagnostic strings exclusively server-side, and return safe, non-leaking messages to clients.
- **Readiness draining lifecycle:** In `rume/src/app/builder.rs:1152-1163`, the shutdown signal immediately transitions availability to `AvailabilityState::RefusingTraffic` before HTTP draining completes. As verified in `rume/src/routing/health.rs:327-341`, `/health/ready` immediately returns 503 to load balancers while in-flight requests finish draining.
- **Non-blocking tracing spans:** `ObservabilityService` (`rume/src/app/observability.rs:297-298`) instruments the request future using `.instrument(span)` without entering span guards across `.await` points, avoiding Tokio task scheduler deadlocks.
- **Atomic transaction rollback on drop:** `MemoryTransaction` (`rume/src/persistence/backend.rs:360-372`) safely rolls back staged mutations on drop or unwinding if the transaction was not explicitly committed. Commit uses `compare_exchange` to guarantee atomicity and prevent double-commit races.
- **Exact saga reverse compensation:** `Saga::run` (`rume/src/saga/coordinator.rs:88-124`) tracks successfully completed step indices and, upon step failure, executes compensations in strictly reverse chronological order. Unexecuted steps and the failing step itself are not compensated, and compensation errors short-circuit to `SagaStatus::CompensationFailed`.
- **Monotonic distributed lock fencing:** `MemoryDistributedLock` (`rume/src/coordination/lock.rs:91-112`) issues monotonically increasing fencing tokens across all lock acquisitions and lease expirations, providing the necessary fencing primitives to prevent split-brain storage writes.
- **Application-scoped route registry:** `RouteRegistry` is now fully encapsulated within `AppContext` and `Router`, eliminating global mutex state and ensuring test isolation across parallel test suites.

---

## Verdict

**Fail — conditional.**

The codebase is substantially improved over v01, with all prior critical defects resolved. However, it cannot be certified as ready for production implementation until **1 Critical** and **3 Major** findings are resolved:

1. **C1** — Prevent job worker shutdown hangs and state leaks by ensuring `in_flight_count` decrements via an RAII drop guard and catching task panics.
2. **M1** — Record failures in `CircuitBreakerService` on transport/network `Err` outcomes, not just 5xx HTTP responses.
3. **M2** — Enforce `Job::timeout()` during worker dispatch so hanging jobs do not lock tasks indefinitely.
4. **M3** — Refuse to unmark migrations in `Migrator::rollback_last` when `down_sql` is `None`.

Once C1 and M1–M3 are fixed, Rume's runtime state machines, lifecycle sequencing, graceful shutdown, and error contracts will be sound and robust.
