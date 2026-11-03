# Jobs & Worker Concurrency Diff Review — v03 (v01)

## Summary

This diff review evaluates the background jobs and worker concurrency subsystem in Rume across commits `cf82559..HEAD`, auditing `rume/src/jobs/` (`queue.rs`, `worker.rs`, `postgres.rs`, `redis.rs`, `job.rs`, `retry.rs`, `reliability.rs`), SQL queries and schemas (`rume/src/persistence/sql/postgres/jobs_*.sql`), and container test suites (`rume/tests/containers/jobs.rs`, `rume/tests/containers/job_reliability.rs`). The vast majority of the critical concurrency and data-loss vulnerabilities identified in v03 have been resolved: strict lease token fencing (`WHERE lease_token = $3 AND status = 'in_flight'`) is enforced across `ack()`, `nack()`, and `dead_letter()`; completed jobs are retained with soft status (`status = 'completed'`) preserving partial unique index deduplication; active lease heartbeat renewals (`renew_lease`) prevent long-running tasks from being stolen; and graceful cancellation tokens are propagated through `JobContext`. However, the change **fails** diff review and cannot be accepted into production because the remediation for job handler panic isolation (v03 M1) is defective: `std::panic::catch_unwind` wraps only the future's construction closure rather than its asynchronous execution, allowing handler panics to unwind uncaught, abandon jobs indefinitely in `in_flight` status without DLQ recording, and break `test_worker_panic_does_not_leak_in_flight_count` in the test suite (Finding M1). In addition, `test_worker_heartbeat_and_abandoned_job_recovery` in the container reliability suite suffers from an inverted startup race that causes deterministic test failures (Finding M2).

---

## Prior decisions

- **v03 Jobs C1 (Job state mutations (`ack`, `nack`, `dead_letter`) lack lease token and worker ownership verification)** — **RESOLVED**.
  `JobQueue` trait signatures in `rume/src/jobs/queue.rs:37–54` now mandate `lease_token: &str`:
  ```rust
  fn ack(&self, job_id: &str, lease_token: &str) -> BoxFuture<'_, Result<(), JobError>>;
  fn nack(&self, job: QueuedJob, lease_token: &str, delay: Duration) -> BoxFuture<'_, Result<(), JobError>>;
  fn dead_letter(&self, job: QueuedJob, lease_token: &str, reason: String) -> BoxFuture<'_, Result<(), JobError>>;
  ```
  In `rume/src/persistence/sql/postgres/jobs_claim.sql:5`, a cryptographic UUID `lease_token` is generated per claim and stored in `rume_jobs.lease_token`. In `rume/src/jobs/postgres.rs:224–301`, `ack()`, `nack()`, and `dead_letter()` enforce `WHERE id = $X AND lease_token = $Y AND status = 'in_flight'` and assert `res.rows_affected() > 0`. If another worker has re-claimed an expired job, any mutation by the superseded worker fails with `JobError::Fatal("cannot ...: lease lost, expired, or invalid token")`.

- **v03 Jobs C2 (Missing job-level lease renewal / heartbeat API causes duplicate concurrent execution of legitimate long-running jobs)** — **RESOLVED**.
  `renew_lease(&self, job_id: &str, lease_token: &str, extension: Duration) -> BoxFuture<'_, Result<(), JobError>>` was added to `JobQueue` (`queue.rs:57–64`) and implemented in `PostgresJobQueue` (`postgres.rs:304–335`) with strict validation (`WHERE id = $2 AND lease_token = $3 AND status = 'in_flight' AND lease_expires_at > $4`). In `JobWorker::execute_job` (`worker.rs:344–364`), an active background task is spawned using `tokio::time::interval(Duration::from_secs(20))` to renew the 60-second claim lease every 20 seconds throughout handler execution, with teardown on job completion.

- **v03 Jobs C3 (`ack()` physically deletes jobs upon completion, destroying PostgreSQL partial unique index deduplication)** — **RESOLVED**.
  Physical deletion via `DELETE FROM rume_jobs` was replaced with a soft completion transition:
  ```sql
  UPDATE rume_jobs
  SET status = 'completed', completed_at = $1, lease_expires_at = NULL
  WHERE id = $2 AND lease_token = $3 AND status = 'in_flight'
  ```
  In `rume/src/persistence/sql/postgres/jobs_schema.sql:18–26`, `completed_at BIGINT` was added, and the unique index was updated to:
  ```sql
  CREATE UNIQUE INDEX IF NOT EXISTS idx_rume_jobs_idempotency
  ON rume_jobs (COALESCE(tenant_id, ''), idempotency_key)
  WHERE idempotency_key IS NOT NULL AND status NOT IN ('dead_letter', 'purged');
  ```
  `jobs_insert.sql:7` mirrors this clause (`WHERE idempotency_key IS NOT NULL AND status NOT IN ('dead_letter', 'purged') DO NOTHING`). Completed jobs retain their idempotency records in PostgreSQL, guaranteeing deduplication across client redeliveries.

- **v03 Jobs M1 (Job handler panic abandons in-flight jobs without DLQ recording and leaves idempotency locks permanently held)** — **CHALLENGED (Defective Remediation)**.
  Commit `77c4db3` attempted to resolve this in `rume/src/jobs/worker.rs:366–371` by wrapping `registry.dispatch_with_cancellation` in `std::panic::catch_unwind`. However, because `dispatch_with_cancellation` returns an asynchronous `BoxFuture`, `catch_unwind` wraps only the future's instantiation closure, while `fut.await` is evaluated outside `catch_unwind`. An asynchronous panic inside a job handler unwinds uncaught through `execute_job`, leaving the job permanently locked in `status = 'in_flight'`, leaking the heartbeat task, skipping idempotency lock release, and causing `worker_panic_does_not_leak_in_flight_count` in `rume/tests/containers/jobs.rs:340` to fail. Re-reported as Finding M1.

- **v03 Jobs M2 (Releasing idempotency keys on retryable failures permits concurrent duplicate execution during the backoff delay)** — **RESOLVED**.
  In `rume/src/jobs/worker.rs:392–402`, `store.release(key).await` was removed from the retryable branch. When `job.retry_policy.next_backoff(job.attempt)` returns `Some(backoff)`, the worker calls only `queue.nack(job, &token, backoff)`. The idempotency key in Redis remains locked in `IN_PROGRESS` throughout the backoff interval, preventing concurrent submissions from executing duplicate tasks. The lock is released only when retries are exhausted and the job transitions to `dead_letter` (line 397).

- **v03 Jobs M3 (`IdempotencyStatus::InProgress` silently drops jobs from the worker without acknowledgment or delayed re-queue)** — **RESOLVED**.
  In `rume/src/jobs/worker.rs:336–339`, the empty `return;` was replaced with an explicit delayed re-queue:
  ```rust
  Ok(IdempotencyStatus::InProgress) => {
      let _ = queue.nack(job, &token, Duration::from_secs(2)).await;
      return;
  }
  ```
  Jobs encountering a concurrent in-flight claim are cleanly returned to `status = 'pending'` with a 2-second backoff delay rather than stranded in `status = 'in_flight'` until lease expiration.

- **v03 Jobs M4 (Non-retryable error types (`UnknownJob`, `Serialization`) are treated as retryable, exhausting max retries before DLQ)** — **RESOLVED**.
  In `rume/src/jobs/worker.rs:383–390`, non-retryable error variants are explicitly pattern-matched alongside `JobError::Fatal`:
  ```rust
  Err(JobError::Fatal(msg))
  | Err(JobError::UnknownJob(msg))
  | Err(JobError::Serialization(msg)) => {
      if let (Some(store), Some(key)) = (&idempotency_store, &effective_key) {
          let _ = store.release(key).await;
      }
      let _ = queue.dead_letter(job, &token, msg).await;
  }
  ```
  Unknown job handlers and corrupted payloads route directly to the dead-letter queue without burning retry attempts or wasting execution permits.

- **v03 Jobs M5 (Redis `IdempotencyStore::release` and `complete` perform unverified mutations, risking cross-worker lock deletion)** — **RESOLVED (Partially)**.
  In `rume/src/jobs/redis.rs:284–302, 318–334`, `complete()` and `release()` were converted to atomic Lua scripts verifying that the key's current value is `'IN_PROGRESS'` before updating or deleting:
  ```lua
  local current = redis.call('GET', KEYS[1])
  if current == 'IN_PROGRESS' then
      redis.call('DEL', KEYS[1])
      return 1
  else
      return 0
  end
  ```
  This prevents a slow or failed worker from deleting a `COMPLETED:` cache entry. (Note: Because `IdempotencyStore` trait methods do not take a unique owner token, a worker whose key expired can still delete a successor worker's `IN_PROGRESS` lock, tracked as minor finding m1).

- **v03 Jobs M6 (`PostgresJobQueue::enqueue_delayed` returns fictitious IDs on duplicate conflict resolution race)** — **RESOLVED**.
  In `rume/src/jobs/postgres.rs:126–144`, if an `ON CONFLICT DO NOTHING` collision occurs and the subsequent fallback `SELECT id` returns `None` (because the conflicting row was removed or transitioned to dead-letter concurrently), `enqueue_delayed` no longer returns the generated phantom UUID. Instead, it returns an explicit error:
  ```rust
  Err(JobError::Fatal("idempotency conflict race: conflicting row is no longer active".into()))
  ```

- **v03 Jobs m1 (Wall-clock underflow panic in `PostgresJobQueue::dequeue_for` on systems with low monotonic uptime)** — **RESOLVED**.
  In `rume/src/jobs/postgres.rs:194–198`, the unsafe subtraction `Instant::now() - Duration::from_millis(...)` was eliminated. When `available_at_ms <= now`, the queue sets `available_at = Instant::now()`, preventing monotonic clock underflow panics on newly booted hosts.

- **v03 Jobs m2 (Exponential backoff in `RetryPolicy` lacks randomized jitter and is vulnerable to NaN/overflow)** — **RESOLVED**.
  In `rume/src/jobs/retry.rs:68–81`, `backoff_factor` is validated with `self.backoff_factor.is_finite() && self.backoff_factor >= 1.0` (defaulting to `2.0` if invalid). Multiplier exponentiation checks `factor_pow.is_finite()`, capping at `self.max_backoff_ms` on float overflow or NaN.

- **v03 Jobs m3 (`JobWorker` lacks cancellation token propagation to `JobContext` during graceful drain)** — **RESOLVED**.
  `JobContext` in `rume/src/jobs/job.rs:56–82` now includes `pub cancellation: Option<Arc<AtomicBool>>` with `is_cancelled(&self) -> bool`. In `rume/src/jobs/worker.rs:290, 295, 367`, `self.is_stopping` is forwarded through `registry.dispatch_with_cancellation`, allowing long-running handlers to observe graceful drain requests.

- **v03 Jobs m4 (DLQ lacks failure timestamp and query/replay administrative APIs)** — **RESOLVED**.
  `rume_jobs` schema added `dead_lettered_at BIGINT` and index `idx_rume_jobs_dead_letter` (`jobs_schema.sql:18, 25`). `JobQueue` introduced `list_dead_letters(limit)` and `replay_dead_letter(job_id)` in `queue.rs:73–86`, fully implemented with parameterized SQL in `PostgresJobQueue` (`postgres.rs:377–448`).

- **v03 Jobs m5 (Worker poll error loop busy-spins every 50ms during database outages)** — **RESOLVED**.
  In `rume/src/jobs/worker.rs:275, 284, 299, 304–308`, `consecutive_errors` tracks consecutive database poll failures, applying exponential backoff:
  ```rust
  let backoff_ms = (50u64 * 2u64.pow(consecutive_errors.min(6))).min(5000);
  sleep(Duration::from_millis(backoff_ms)).await;
  ```
  Poller sleep scales from 100ms up to 3200ms and caps at 5000ms, resetting to 0 on any successful dequeue.

- **v03 Jobs m6 (Dual lease expiration recovery between `jobs_claim.sql` and `jobs_reap.sql` lacks coordination)** — **RESOLVED**.
  In `rume/src/persistence/sql/postgres/jobs_reap.sql:1–11`, `lease_expires_at <= $2` was excised, and `FOR UPDATE SKIP LOCKED` was added. Reaping is now strictly restricted to dead worker nodes (`assigned_worker = ANY($1)`), while claim lease expiration recovery is consolidated solely into `jobs_claim.sql`.

---

## Findings

### M1 Unhandled asynchronous job handler panics bypass `catch_unwind`, stranding jobs in `in_flight` status and breaking DLQ routing

- **Where:** `rume/src/jobs/worker.rs:366–371`, `rume/tests/containers/jobs.rs:319–342`
- **What it says:**
  In `rume/src/jobs/worker.rs`:
  ```rust
  let dispatch_result = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
      registry.dispatch_with_cancellation(&job, Some(stopping))
  })) {
      Ok(fut) => fut.await,
      Err(_) => Err(JobError::Fatal("handler panicked".into())),
  };
  ```
  and `registry.dispatch_with_cancellation` (`worker.rs:72–97`):
  ```rust
  pub async fn dispatch_with_cancellation(
      &self,
      job: &QueuedJob,
      cancellation: Option<Arc<AtomicBool>>,
  ) -> Result<(), JobError> {
      ...
      let fut = handler(&ctx, &job.payload);
      if let Some(timeout) = job.timeout {
          match tokio::time::timeout(timeout, fut).await {
              Ok(res) => res,
              Err(_) => Err(JobError::Timeout),
          }
      } else {
          fut.await
      }
  }
  ```
- **Why it's a problem:**
  In Rust's async execution model, invoking an asynchronous function or closure does not execute the async code; it merely instantiates a `Future` state machine.
  
  Wrapping `std::panic::catch_unwind` synchronously around `|| { registry.dispatch_with_cancellation(&job, Some(stopping)) }` intercepts panics only if they occur during the synchronous construction of the future. The actual handler logic is executed when `fut.await` is evaluated on line 369. Because `fut.await` is positioned *outside* of `catch_unwind`, any panic occurring inside user async handler code unwinds completely uncaught through `execute_job`.
  
  When an unhandled panic occurs (such as an unexpected `.unwrap()` or panic assertion):
  1. Unwinding aborts `execute_job` at line 369.
  2. Lines 373–404 are skipped entirely: `cancel_tx.send(())` is skipped, `heartbeat_handle.abort()` is skipped, and the error match arm is never entered.
  3. The job is never transitioned to `dead_letter` via `queue.dead_letter()`. In PostgreSQL, the job row remains permanently stuck in `status = 'in_flight'`.
  4. Any held Redis idempotency lock (`store.try_acquire`) is never released via `store.release()`, locking out subsequent submissions for 300 seconds.
  5. The spawned heartbeat task continues running in the background until the receiver channel is dropped.
  6. The container integration test `jobs::worker_panic_does_not_leak_in_flight_count` in `rume/tests/containers/jobs.rs:319–342` asserts this durable queue invariant:
     ```rust
     assert_eq!(queue.dead_letter_count().await.unwrap(), 1, "panicked job must be routed to DLQ");
     ```
     Running `cargo test --test containers --features postgres -- jobs::worker_panic_does_not_leak_in_flight_count` fails deterministically on `HEAD`:
     ```
     thread 'jobs::worker_panic_does_not_leak_in_flight_count' panicked at rume/tests/containers/jobs.rs:340:5:
     assertion `left == right` failed: panicked job must be routed to DLQ
       left: 0
      right: 1
     ```
- **Suggested fix:**
  Catch unwinding panics during the polling/awaiting of the future itself. Either add `futures-util = { workspace = true }` to `rume/Cargo.toml` and use `FutureExt::catch_unwind`:
  ```rust
  use futures_util::FutureExt;

  let dispatch_result = match std::panic::AssertUnwindSafe(
      registry.dispatch_with_cancellation(&job, Some(stopping))
  )
  .catch_unwind()
  .await
  {
      Ok(res) => res,
      Err(panic_payload) => {
          let msg = if let Some(s) = panic_payload.downcast_ref::<&str>() {
              s.to_string()
          } else if let Some(s) = panic_payload.downcast_ref::<String>() {
              s.clone()
          } else {
              "unknown panic".to_string()
          };
          Err(JobError::Fatal(format!("handler panicked: {msg}")))
      }
  };
  ```
  Or execute the dispatcher future inside a spawned Tokio task (`tokio::spawn`) and check `join_err.is_panic()`.

---

### M2 Race condition and uncoordinated worker execution in `test_worker_heartbeat_and_abandoned_job_recovery` causes deterministic container test failure

- **Where:** `rume/tests/containers/job_reliability.rs:227–258`
- **What it says:**
  ```rust
  // Worker 1 starts with heartbeat and abandoned recovery
  let worker1 = rume::jobs::JobWorker::new(Arc::clone(&queue), registry)
      .with_worker_id("worker-active-1")
      .with_heartbeat(
          Arc::clone(&heartbeats),
          Duration::from_millis(400),
          Duration::from_millis(100),
      )
      .with_abandoned_recovery(Duration::from_millis(200));
  worker1.start();

  // Verify worker 1 registered heartbeat
  sleep(Duration::from_millis(150)).await;
  assert!(heartbeats.is_alive("worker-active-1").await.unwrap());

  // Simulate an abandoned job belonging to a crashed worker
  heartbeats
      .record_heartbeat("crashed-worker-99", Duration::from_millis(100))
      .await
      .unwrap();

  // Enqueue and claim a job under crashed-worker-99
  queue
      .enqueue(IdempotentPaymentJob {
          payment_id: "tx_abandoned".into(),
          amount: 100,
      })
      .await
      .unwrap();
  let claimed = queue.dequeue_for("crashed-worker-99").await.unwrap();
  assert!(claimed.is_some());
  ```
- **Why it's a problem:**
  In `test_worker_heartbeat_and_abandoned_job_recovery`, `worker1.start()` is invoked on line 234, spawning `worker1`'s background execution loop `run_loop()` which polls `queue.dequeue_for("worker-active-1")` every 25ms.
  
  On line 249, the test enqueues `IdempotentPaymentJob` into `queue`. Because `worker1` is already actively running and polling, `worker1` claims the newly enqueued job almost instantly.
  
  When line 255 subsequently invokes `queue.dequeue_for("crashed-worker-99")`, the job is already claimed and in `status = 'in_flight'` under `worker-active-1` with a valid 60-second lease. `jobs_claim.sql` finds 0 matching rows, and `dequeue_for` returns `Ok(None)`.
  
  Line 256 panics immediately:
  ```
  thread 'job_reliability::test_worker_heartbeat_and_abandoned_job_recovery' panicked at rume/tests/containers/job_reliability.rs:256:5:
  assertion failed: claimed.is_some()
  ```
  Furthermore, because `worker1` was initialized with an empty registry (`let registry = Arc::new(rume::jobs::JobRegistry::new());`), when `worker1` steals the job, `dispatch` returns `JobError::UnknownJob`, which immediately routes the stolen job to `dead_letter`. The abandoned job recovery logic on lines 260–263 is never exercised. This test fails 100% of the time when running `cargo test --test containers --features postgres,redis -- test_worker_heartbeat_and_abandoned_job_recovery`.
- **Suggested fix:**
  Set up the abandoned job fixture under `crashed-worker-99` *before* starting `worker1`, and register a handler or coordinate polling so `worker1` does not immediately consume the reclaimed job:
  ```rust
  // 1. Simulate an abandoned job belonging to a crashed worker
  heartbeats
      .record_heartbeat("crashed-worker-99", Duration::from_millis(100))
      .await
      .unwrap();

  // 2. Enqueue and claim under crashed-worker-99 BEFORE starting worker1
  queue
      .enqueue(IdempotentPaymentJob {
          payment_id: "tx_abandoned".into(),
          amount: 100,
      })
      .await
      .unwrap();
  let claimed = queue.dequeue_for("crashed-worker-99").await.unwrap();
  assert!(claimed.is_some());
  assert_eq!(queue.pending_count().await.unwrap(), 0);

  // 3. Now start worker1 to reap the dead worker
  let worker1 = rume::jobs::JobWorker::new(Arc::clone(&queue), registry)
      .with_worker_id("worker-active-1")
      .with_heartbeat(
          Arc::clone(&heartbeats),
          Duration::from_millis(400),
          Duration::from_millis(100),
      )
      .with_abandoned_recovery(Duration::from_millis(200));
  worker1.start();
  ```

---

### m1 Redis `IdempotencyStore::release` and `complete` perform unverified mutations across expired lease renewals

- **Where:** `rume/src/jobs/redis.rs:284–302, 318–334`
- **What it says:**
  In `rume/src/jobs/redis.rs`:
  ```lua
  local current = redis.call('GET', KEYS[1])
  if current == 'IN_PROGRESS' then
      redis.call('DEL', KEYS[1])
      return 1
  else
      return 0
  end
  ```
- **Why it's a problem:**
  The Lua scripts verify that the current key status is `'IN_PROGRESS'`. This effectively prevents a superseded or slow worker from overwriting or deleting a `COMPLETED:` record.
  
  However, neither `try_acquire` nor `complete`/`release` accepts an owner token (e.g. `owner_token = Uuid::new_v4().to_string()`). If Worker A acquires key `K` with a 300s TTL, stalls for 305s, and Worker B subsequently acquires key `K` (setting `IN_PROGRESS`), Worker A waking up and calling `release(K)` will see `current == 'IN_PROGRESS'` (held by Worker B) and delete Worker B's active lock.
- **Suggested fix:**
  Store `IN_PROGRESS:<token>` in Redis, return the token from `try_acquire`, and verify that `redis.call('GET', KEYS[1]) == ARGV[1]` in the Lua scripts.

---

### m2 Job lease renewal failure in `JobWorker` heartbeat does not signal cancellation to running job handler

- **Where:** `rume/src/jobs/worker.rs:356–361`
- **What it says:**
  ```rust
  if let Err(e) = hb_queue.renew_lease(&hb_jid, &hb_token, Duration::from_secs(60)).await {
      tracing::warn!(job_id = %hb_jid, error = %e, "Failed to renew job lease heartbeat");
      break;
  }
  ```
- **Why it's a problem:**
  When `renew_lease` returns an error (which indicates that the claim lease has expired and been stolen by a peer worker, or the database rejected the token), the heartbeat task logs a warning and breaks out of its loop.
  
  However, the running job handler is not notified of the lease loss. It continues executing expensive database writes, external API calls, or computations until completion. When it finally completes, its `queue.ack(&job_id, &token)` is rejected because the lease token is superseded.
- **Suggested fix:**
  When `renew_lease` fails, trigger cancellation on the job context (e.g., via a job-specific cancellation signal or setting `is_stopping`) so cooperative handlers can abort early.

---

## Positive notes

- **Strict lease fencing (`WHERE lease_token = $3 AND status = 'in_flight'`)**: `ack`, `nack`, and `dead_letter` now validate the unique lease token and verify row count, eliminating ghost deliveries and race conditions from superseded workers.
- **Retention of completed jobs with soft status (`status = 'completed'`)**: Replacing physical `DELETE` with `status = 'completed'` and `completed_at` preserves PostgreSQL partial unique index deduplication across idempotent submissions.
- **First-class DLQ administration and replay APIs**: `list_dead_letters` and `replay_dead_letter` provide full visibility, ordering by `dead_lettered_at DESC` and atomic reactivation to `pending`.
- **Active lease renewal loop**: Automatic background renewal in `JobWorker` every 20 seconds extends active claims to 60 seconds, preventing peer workers from re-driving long-running jobs.
- **Graceful shutdown cancellation awareness**: `JobContext::is_cancelled()` allows long-running tasks to respond to worker drain signals.
- **Multi-tenant queue and idempotency scoping**: Support for `tenant_id` in `Job`, schema indexing on `(COALESCE(tenant_id, ''), idempotency_key)`, and length-prefixed key scoping prevents cross-tenant collisions.
- **Safe error classification and exponential poll backoff**: Non-retryable errors (`UnknownJob`, `Serialization`) bypass retries and route directly to DLQ, and worker polling backs off exponentially during database errors.

---

## Verdict

**Fail** (Must land fix pass first).

The background jobs and worker concurrency subsystem has successfully resolved 14 of the 15 prior findings, including all three Critical findings (C1, C2, C3). However, the diff cannot be accepted into production because the remediation for Finding M1 is defective: asynchronous handler panics bypass `std::panic::catch_unwind`, stranding jobs indefinitely in `status = 'in_flight'` and causing `jobs::worker_panic_does_not_leak_in_flight_count` in the test suite to fail (Finding M1). Additionally, `job_reliability::test_worker_heartbeat_and_abandoned_job_recovery` suffers from a startup race that deterministically fails the container test suite (Finding M2). A fix pass resolving **Finding M1** and **Finding M2** must land before this diff can be merged.
