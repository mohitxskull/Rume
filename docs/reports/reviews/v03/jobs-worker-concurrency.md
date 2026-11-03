# Jobs & Worker Concurrency Review — v03

## Summary

The Rume background jobs subsystem introduces robust foundations for distributed asynchronous execution: PostgreSQL-backed durable queueing with row-level `FOR UPDATE SKIP LOCKED` semantics, RAII-guarded worker concurrency limits using Tokio semaphores, per-job execution timeouts, and pluggable Redis-backed worker heartbeats and idempotency storage. However, the subsystem is not ready for production because of critical concurrency and state ownership vulnerabilities: job claim state mutations (`ack`, `nack`, `dead_letter`) lack lease token and worker ownership verification, allowing superseded workers to delete or corrupt jobs actively owned by peer workers; there is no active job lease renewal or heartbeat API, causing legitimate long-running jobs to be re-driven concurrently while still executing; `ack()` physically deletes database rows, destroying PostgreSQL-level idempotency deduplication; unhandled panics abandon jobs without DLQ recording; and releasing idempotency keys on retryable failures permits concurrent duplicate runs during backoff delays.

---

## Prior decisions

- **v02 Correctness C1 (Panic during job execution permanently leaks `in_flight_count` and hangs `drain()`)** — **CHALLENGED (Partial Remediation)**. The RAII drop guard `InFlightGuard` (`rume/src/jobs/worker.rs:89–95`) successfully decrements `in_flight_count` during task unwinding, preventing `JobWorker::drain()` from hanging. However, the second requirement of the prior fix—catching task panics via `catch_unwind` and transitioning the job to the dead-letter queue—was not implemented. Unhandled panics escape `execute_job`, leaving the job orphaned in `in_flight` status in PostgreSQL, failing to release or dead-letter the job, and leaving idempotency locks held until TTL. Evaluated in finding M1.
- **v02 Correctness M2 (`Job::timeout()` is never enforced during job execution)** — **REAFFIRMED**. Verified in `rume/src/jobs/worker.rs:77–82`, `rume/src/jobs/job.rs:69`, and `rume/src/jobs/postgres.rs:184`. Job timeouts are preserved in `QueuedJob.timeout` and strictly enforced during handler dispatch via `tokio::time::timeout`, converting exceeded deadlines into `JobError::Timeout`.
- **v02 Enterprise-Reliability M1 (`JobWorker` lacks shed-load concurrency bounds)** — **REAFFIRMED**. Verified in `rume/src/jobs/worker.rs:104–105, 156–175, 264–267`. `JobWorker` enforces bounded concurrency defaulting to 50 tasks (or custom limits) using an owned `tokio::sync::Semaphore` permit acquired prior to `dequeue_for`.
- **v02 Enterprise-Reliability M2 (Distributed worker heartbeats and abandoned job recovery are disconnected dead code)** — **CHALLENGED**. While `JobWorker::start` now spawns periodic worker node heartbeat and abandoned worker recovery loops (`rume/src/jobs/worker.rs:229–258`), the implementation operates exclusively at the coarse *worker node* level (`RedisHeartbeatRegistry`) rather than the *job claim lease* level. A healthy worker running a legitimate job that exceeds the 60-second database claim lease duration has no lease renewal API, resulting in peer workers reclaiming and duplicating the job while the first worker is still active. Furthermore, worker heartbeats live in Redis while jobs live in PostgreSQL, creating split-brain divergence. Evaluated in findings C1 and C2.
- **v02 Testing M1 (Happy-path test bias across newly introduced subsystems)** — **REAFFIRMED**. Tests in `rume/tests/jobs_suite.rs` and `rume/tests/job_reliability_suite.rs` verify retry backoff, DLQ on max attempts, timeout enforcement, bounded concurrency, and worker panic drop safety. However, concurrent race tests for superseded worker lease theft, worker crash mid-retry, and unacknowledged in-progress idempotency skips are missing.
- **PostgreSQL Durable Job Queue (`PostgresJobQueue`) & Redis Reliability Store (`RedisIdempotencyStore`)** — **NOTE-NEW**. New production implementations introduced in the Phase 5 / SQLx modernization; audited in findings C1, C2, C3, M2, M3, M5, M6, m1, and m6.

---

## Findings

### C1 Job state mutations (`ack`, `nack`, `dead_letter`) lack lease token and worker ownership verification, enabling superseded workers to corrupt or delete active jobs

- **Where:** `rume/src/jobs/queue.rs:37–45` and `rume/src/jobs/postgres.rs:209–252`
- **What it says:**
  In `rume/src/jobs/queue.rs`:
  ```rust
  /// Acknowledges successful execution, removing the job from the queue.
  fn ack(&self, job_id: &str) -> BoxFuture<'_, Result<(), JobError>>;

  /// Re-queues a failed job after a backoff delay.
  fn nack(&self, job: QueuedJob, delay: Duration) -> BoxFuture<'_, Result<(), JobError>>;

  /// Moves a permanently failed job to the dead-letter queue (DLQ).
  fn dead_letter(&self, job: QueuedJob, reason: String) -> BoxFuture<'_, Result<(), JobError>>;
  ```
  In `rume/src/jobs/postgres.rs`:
  ```rust
  fn ack(&self, job_id: &str) -> BoxFuture<'_, Result<(), JobError>> {
      let jid = job_id.to_string();
      Box::pin(async move {
          sqlx::query("DELETE FROM rume_jobs WHERE id = $1")
              .bind(&jid)
              .execute(&self.pool)
              .await
              .map_err(|e| JobError::Fatal(e.to_string()))?;
          Ok(())
      })
  }

  fn nack(&self, job: QueuedJob, delay: Duration) -> BoxFuture<'_, Result<(), JobError>> {
      Box::pin(async move {
          let now = current_epoch_millis();
          let next_available = now.saturating_add(delay.as_millis() as i64);

          sqlx::query(
              "UPDATE rume_jobs SET status = 'pending', assigned_worker = NULL, lease_expires_at = NULL, available_at = $1 WHERE id = $2"
          )
          .bind(next_available)
          .bind(&job.id)
          .execute(&self.pool)
          .await
          .map_err(|e| JobError::Fatal(e.to_string()))?;

          Ok(())
      })
  }

  fn dead_letter(&self, job: QueuedJob, reason: String) -> BoxFuture<'_, Result<(), JobError>> {
      Box::pin(async move {
          sqlx::query(
              "UPDATE rume_jobs SET status = 'dead_letter', assigned_worker = NULL, lease_expires_at = NULL, dead_letter_reason = $1 WHERE id = $2"
          )
          .bind(&reason)
          .bind(&job.id)
          .execute(&self.pool)
          .await
          .map_err(|e| JobError::Fatal(e.to_string()))?;

          Ok(())
      })
  }
  ```
- **Why it's a problem:** None of the mutation operations (`ack`, `nack`, `dead_letter`) verify that the calling worker currently owns the job claim, nor do they check that the worker's claim lease has not expired.
  
  **Concrete failure scenario:**
  1. Worker A claims Job 1 with a default 60-second lease (`assigned_worker = 'worker-A'`, `lease_expires_at = T0 + 60s`).
  2. Worker A encounters an external API stall, database lock delay, or thread pause lasting 65 seconds.
  3. At $T_0 + 60\text{s}$, Worker A's claim lease expires in PostgreSQL.
  4. Worker B calls `dequeue_for("worker-B")`. In `jobs_claim.sql`, the query matches `(status = 'in_flight' AND lease_expires_at <= now)`. Worker B claims Job 1, setting `assigned_worker = 'worker-B'` and `lease_expires_at = T0 + 120s`. Worker B begins executing Job 1.
  5. At $T_0 + 65\text{s}$, Worker A finishes its execution of Job 1.
  6. If Worker A succeeded: Worker A calls `queue.ack("1")`. `ack` issues `DELETE FROM rume_jobs WHERE id = '1'`. This unconditionally deletes Job 1 from PostgreSQL while Worker B is actively processing it! When Worker B finishes and calls `ack`, the row is already gone.
  7. If Worker A failed: Worker A calls `nack(job, delay)`. `nack` issues `UPDATE rume_jobs SET status = 'pending', assigned_worker = NULL, lease_expires_at = NULL, available_at = $1 WHERE id = '1'`. This resets Job 1 back to `'pending'` and wipes Worker B's assignment, allowing Worker C to dequeue Job 1 immediately. Worker B and Worker C are now executing Job 1 concurrently!
  8. If Worker A calls `dead_letter()`, it forcibly aborts Worker B's valid execution by marking the job dead-lettered in the database.
- **Suggested fix:**
  1. Add a monotonic `lease_token: String` (or `claim_epoch: i64`) to `rume_jobs` and `QueuedJob`.
  2. In `jobs_claim.sql`, generate a new `lease_token` (e.g. `gen_random_uuid()` or an incrementing counter) on each claim.
  3. Update `JobQueue` signatures to require ownership validation:
     `fn ack(&self, job_id: &str, lease_token: &str) -> BoxFuture<'_, Result<(), JobError>>`
     `fn nack(&self, job: QueuedJob, lease_token: &str, delay: Duration) -> BoxFuture<'_, Result<(), JobError>>`
     `fn dead_letter(&self, job: QueuedJob, lease_token: &str, reason: String) -> BoxFuture<'_, Result<(), JobError>>`
  4. In `PostgresJobQueue`, enforce fencing in the SQL `WHERE` clause:
     `DELETE FROM rume_jobs WHERE id = $1 AND lease_token = $2 AND lease_expires_at > $3`
     If `rows_affected() == 0`, return `Err(JobError::Fatal("claim lease lost: superseded by another worker".into()))`.

---

### C2 Missing job-level lease renewal / heartbeat API causes duplicate concurrent execution of legitimate long-running jobs

- **Where:** `rume/src/jobs/queue.rs:10–51`, `rume/src/jobs/postgres.rs:32, 48–53, 151–159`, and `rume/src/jobs/worker.rs:296–343`
- **What it says:**
  In `rume/src/jobs/postgres.rs`:
  ```rust
  pub fn new(pool: PgPool) -> Self {
      Self {
          pool,
          default_lease_duration: Duration::from_secs(60),
      }
  }
  ```
  In `rume/src/jobs/queue.rs`:
  The `JobQueue` trait exposes only `enqueue`, `enqueue_delayed`, `dequeue`, `dequeue_for`, `reclaim_abandoned_jobs`, `ack`, `nack`, `dead_letter`, `pending_count`, and `dead_letter_count`. There is no `renew_lease` or `heartbeat_job` method.
  In `rume/src/jobs/worker.rs`:
  ```rust
  async fn execute_job(
      queue: Arc<Q>,
      registry: Arc<JobRegistry>,
      job: QueuedJob,
      idempotency_store: Option<Arc<dyn IdempotencyStore>>,
  ) {
      ...
      match registry.dispatch(&job).await {
          Ok(()) => { let _ = queue.ack(&job_id).await; }
          ...
      }
  }
  ```
- **Why it's a problem:** In `PostgresJobQueue`, claims expire after `default_lease_duration` (60 seconds). However, background jobs frequently execute tasks that legitimately require several minutes (e.g. video rendering, generating PDF reports, bulk tenant data export, ML inference batching). The `Job` trait explicitly supports configurable timeouts via `Job::timeout()` (e.g. `Some(Duration::from_secs(300))`).
  
  Because there is no lease heartbeat or renewal mechanism:
  1. A job configured with a 5-minute timeout begins execution on Worker 1.
  2. At T = 60s, while Worker 1 is still actively and correctly processing the job, PostgreSQL marks its claim lease expired (`status = 'in_flight' AND lease_expires_at <= now`).
  3. Worker 2 calls `dequeue()` and claims the exact same job via `jobs_claim.sql`. Worker 2 increments `attempt` and begins executing the job concurrently with Worker 1.
  4. If the job performs external side effects (charging payment gateways, sending transactional SMS, writing to S3), the operation is duplicated.
  5. When Worker 1 finishes at T = 180s, its `ack` unconditionally deletes the job from under Worker 2 (per finding C1).
- **Suggested fix:**
  1. Add `renew_lease` to the `JobQueue` trait:
     ```rust
     fn renew_lease(
         &self,
         job_id: &str,
         lease_token: &str,
         extension: Duration,
     ) -> BoxFuture<'_, Result<(), JobError>>;
     ```
  2. Implement `renew_lease` in `PostgresJobQueue`:
     `UPDATE rume_jobs SET lease_expires_at = $1 WHERE id = $2 AND lease_token = $3 AND status = 'in_flight'`.
  3. In `JobWorker::execute_job`, spawn an active heartbeat renewal task that periodically calls `renew_lease` (e.g. every `lease_duration / 3`) while `registry.dispatch(&job)` is running. If `renew_lease` returns an error or indicates the lease was lost, cancel the running handler task immediately.

---

### C3 `ack()` physically deletes jobs upon completion, destroying PostgreSQL partial unique index deduplication and causing duplicate execution on redelivery

- **Where:** `rume/src/jobs/postgres.rs:209–219`, `rume/src/persistence/sql/postgres/jobs_schema.sql:21`, and `rume/src/persistence/sql/postgres/jobs_insert.sql:6–7`
- **What it says:**
  In `rume/src/persistence/sql/postgres/jobs_schema.sql`:
  ```sql
  CREATE UNIQUE INDEX IF NOT EXISTS idx_rume_jobs_idempotency
  ON rume_jobs (idempotency_key)
  WHERE idempotency_key IS NOT NULL AND status != 'dead_letter';
  ```
  In `rume/src/persistence/sql/postgres/jobs_insert.sql`:
  ```sql
  INSERT INTO rume_jobs (...) VALUES (...)
  ON CONFLICT (idempotency_key) WHERE idempotency_key IS NOT NULL AND status != 'dead_letter'
  DO NOTHING
  RETURNING id;
  ```
  In `rume/src/jobs/postgres.rs`:
  ```rust
  fn ack(&self, job_id: &str) -> BoxFuture<'_, Result<(), JobError>> {
      let jid = job_id.to_string();
      Box::pin(async move {
          sqlx::query("DELETE FROM rume_jobs WHERE id = $1")
              .bind(&jid)
              .execute(&self.pool)
              .await
              .map_err(|e| JobError::Fatal(e.to_string()))?;
          Ok(())
      })
  }
  ```
- **Why it's a problem:** The PostgreSQL schema specifically declares a partial unique index `idx_rume_jobs_idempotency` on `idempotency_key` where `status != 'dead_letter'`, and `jobs_insert.sql` uses `ON CONFLICT (idempotency_key) ... DO NOTHING` to guarantee deduplication at the storage layer.
  
  However, upon successful completion, `PostgresJobQueue::ack()` executes `DELETE FROM rume_jobs WHERE id = $1`. Deleting the row completely removes the `idempotency_key` record from the table.
  
  If a webhook provider, transactional outbox relay, or upstream client redelivers the identical request/event minutes later (or during network reconnects), the `INSERT` finds no conflicting row because the original job was deleted. PostgreSQL inserts a new row with status `'pending'`, and the job is executed a second time. If `RedisIdempotencyStore` is not enabled, or if Redis keys have expired, PostgreSQL offers zero persistent idempotency deduplication despite advertising it in the schema.
- **Suggested fix:**
  1. Do not hard-delete jobs in `ack()` when an `idempotency_key` is present. Instead, transition the row to `status = 'completed'` with a `completed_at` timestamp.
  2. Include `'completed'` in the partial unique index:
     `WHERE idempotency_key IS NOT NULL AND status NOT IN ('dead_letter', 'purged')`.
  3. Implement a periodic background cleanup sweep to prune completed jobs older than a configurable retention period (e.g. 7 or 30 days). Alternatively, insert completed keys into a dedicated `rume_job_completions (idempotency_key TEXT PRIMARY KEY, completed_at BIGINT)` table within the same transaction as `DELETE FROM rume_jobs`.

---

### M1 Job handler panic abandons in-flight jobs without DLQ recording and leaves idempotency locks permanently held

- **Where:** `rume/src/jobs/worker.rs:277–281, 296–343`
- **What it says:**
  In `rume/src/jobs/worker.rs`:
  ```rust
  tokio::spawn(async move {
      let _permit = permit;
      let _guard = InFlightGuard(in_flight);
      Self::execute_job(queue, registry, job, idempotency).await;
  });
  ```
  and `execute_job`:
  ```rust
  async fn execute_job(
      queue: Arc<Q>,
      registry: Arc<JobRegistry>,
      job: QueuedJob,
      idempotency_store: Option<Arc<dyn IdempotencyStore>>,
  ) {
      let job_id = job.id.clone();
      ...
      match registry.dispatch(&job).await {
          Ok(()) => { ... }
          Err(JobError::Fatal(msg)) => { ... }
          Err(err) => { ... }
      }
  }
  ```
- **Why it's a problem:** While `InFlightGuard` ensures `in_flight_count` is decremented on panic (addressing the shutdown hang identified in v02 C1), `execute_job` contains no panic boundary (`std::panic::AssertUnwindSafe(...).catch_unwind()`).
  
  When a registered job handler panics (e.g. unexpected `unwrap()`, index out of bounds, slice mismatch):
  1. Unwinding aborts `execute_job` mid-flight.
  2. The job is never acknowledged, re-queued (`nack`), or dead-lettered (`dead_letter`).
  3. In `rume_jobs`, the job remains permanently locked in `status = 'in_flight'` until its lease expires 60 seconds later.
  4. When the lease expires, peer workers re-claim the malformed job, attempt execution, and panic again, creating a cascading worker crash loop.
  5. The idempotency store lock (`store.try_acquire`) is never released (`store.release(key)` is skipped), leaving the key locked in Redis for 300 seconds and blocking manual or automated re-submission.
  6. No dead-letter error record or panic backtrace is captured in the database or surfaced to operational dashboards.
- **Suggested fix:**
  Wrap handler execution in `futures::FutureExt::catch_unwind`:
  ```rust
  let dispatch_result = std::panic::AssertUnwindSafe(registry.dispatch(&job))
      .catch_unwind()
      .await;

  match dispatch_result {
      Ok(Ok(())) => {
          let _ = queue.ack(&job_id).await;
          if let (Some(store), Some(key)) = (&idempotency_store, &job.idempotency_key) {
              let _ = store.complete(key, None).await;
          }
      }
      Ok(Err(err)) => {
          // Normal error handling and retry logic
      }
      Err(panic_payload) => {
          let panic_msg = if let Some(s) = panic_payload.downcast_ref::<&str>() {
              s.to_string()
          } else if let Some(s) = panic_payload.downcast_ref::<String>() {
              s.clone()
          } else {
              "unknown panic".to_string()
          };
          if let (Some(store), Some(key)) = (&idempotency_store, &job.idempotency_key) {
              let _ = store.release(key).await;
          }
          let _ = queue.dead_letter(job, format!("handler panicked: {panic_msg}")).await;
      }
  }
  ```

---

### M2 Releasing idempotency keys on retryable failures permits concurrent duplicate execution during the backoff delay

- **Where:** `rume/src/jobs/worker.rs:330–337`
- **What it says:**
  ```rust
  Err(err) => {
      if let (Some(store), Some(key)) = (&idempotency_store, &job.idempotency_key) {
          let _ = store.release(key).await;
      }
      // Retryable failure: check remaining attempts
      if let Some(backoff) = job.retry_policy.next_backoff(job.attempt) {
          let _ = queue.nack(job, backoff).await;
      } else {
          let reason = format!("max retries exceeded ({}) - error: {err}", job.attempt);
          let _ = queue.dead_letter(job, reason).await;
      }
  }
  ```
- **Why it's a problem:** In `execute_job`, when attempt 1 fails with a retryable error (e.g. transient network timeout or database lock contention), the worker immediately invokes `store.release(key).await` (which executes `DEL` in Redis), and then calls `queue.nack(job, backoff)`.
  
  During the backoff delay (which may be 5, 30, or 60 seconds):
  1. The idempotency key in Redis has been deleted.
  2. If an upstream producer, user retry, or external scheduler enqueues a job with the identical idempotency key during this backoff window, `store.try_acquire(key)` returns `IdempotencyStatus::New`.
  3. A second worker dequeues the new job and immediately executes it.
  4. When the backoff timer for the original job expires, the original job is dequeued and re-executed.
  5. Both jobs now execute in parallel, destroying idempotency guarantees.
- **Suggested fix:**
  Do not release the idempotency key when scheduling a retry. Keep the key in `IN_PROGRESS` or transition it to `RETRY_SCHEDULED`, extending its TTL by the backoff duration. Only release or clear the idempotency lock when the job exhausts all retries and transitions to `dead_letter`, or mark it `COMPLETED` on success.

---

### M3 `IdempotencyStatus::InProgress` silently drops jobs from the worker without acknowledgment or delayed re-queue

- **Where:** `rume/src/jobs/worker.rs:304–315`
- **What it says:**
  ```rust
  if let (Some(store), Some(key)) = (&idempotency_store, &job.idempotency_key) {
      match store.try_acquire(key, Duration::from_secs(300)).await {
          Ok(IdempotencyStatus::Completed(_)) => {
              let _ = queue.ack(&job_id).await;
              return;
          }
          Ok(IdempotencyStatus::InProgress) => {
              return;
          }
          _ => {}
      }
  }
  ```
- **Why it's a problem:** When a worker dequeues a job and `store.try_acquire` returns `IdempotencyStatus::InProgress` (because a peer worker or concurrent task is actively processing the key), the worker immediately exits the function via `return;`.
  
  Because the job was already dequeued via `dequeue_for()`, its row in PostgreSQL is set to `status = 'in_flight'` with `assigned_worker = worker_id`. By exiting without calling `ack` or `nack`, the worker abandons the job in `in_flight` status.
  
  The job remains stuck in `in_flight` for 60 seconds until its claim lease expires. If the concurrent task finishes in 500 milliseconds, this duplicate job is unnecessarily stranded for an entire minute. If the concurrent task fails and releases the key, this job will not retry until lease expiration rather than backing off appropriately.
- **Suggested fix:**
  Instead of an empty `return;`, re-queue the job with a short backoff delay:
  ```rust
  Ok(IdempotencyStatus::InProgress) => {
      // Re-queue with a short delay (e.g. 1-2 seconds) to recheck completion status
      let _ = queue.nack(job, Duration::from_secs(2)).await;
      return;
  }
  ```

---

### M4 Non-retryable error types (`UnknownJob`, `Serialization`) are treated as retryable, exhausting max retries before DLQ

- **Where:** `rume/src/jobs/worker.rs:324–342` and `rume/src/jobs/error.rs:20–26`
- **What it says:**
  In `rume/src/jobs/worker.rs`:
  ```rust
  match registry.dispatch(&job).await {
      Ok(()) => { ... }
      Err(JobError::Fatal(msg)) => {
          if let (Some(store), Some(key)) = (&idempotency_store, &job.idempotency_key) {
              let _ = store.release(key).await;
          }
          let _ = queue.dead_letter(job, msg).await;
      }
      Err(err) => {
          if let (Some(store), Some(key)) = (&idempotency_store, &job.idempotency_key) {
              let _ = store.release(key).await;
          }
          // Retryable failure: check remaining attempts
          if let Some(backoff) = job.retry_policy.next_backoff(job.attempt) {
              let _ = queue.nack(job, backoff).await;
          } else {
              let reason = format!("max retries exceeded ({}) - error: {err}", job.attempt);
              let _ = queue.dead_letter(job, reason).await;
          }
      }
  }
  ```
- **Why it's a problem:** `execute_job` treats only `JobError::Fatal` as immediately fatal. If a job fails with `JobError::UnknownJob(name)` (no handler registered for the job name) or `JobError::Serialization(e)` (corrupted JSON payload that cannot deserialize into the target struct), the error falls through to the generic `Err(err)` arm.
  
  The worker treats these permanent, unrecoverable defects as retryable, scheduling backoff delays and re-attempting execution 3 to 5 times. Retrying an unknown job name or a corrupt payload can never succeed; it wastes database writes, consumes worker thread permits, delays error discovery, and fills logs with redundant errors before finally reaching DLQ.
- **Suggested fix:**
  Route all deterministic, non-retryable errors directly to `dead_letter`:
  ```rust
  match registry.dispatch(&job).await {
      Ok(()) => { ... }
      Err(JobError::Fatal(msg)) => {
          ...
          let _ = queue.dead_letter(job, msg).await;
      }
      Err(err @ JobError::UnknownJob(_)) | Err(err @ JobError::Serialization(_)) => {
          if let (Some(store), Some(key)) = (&idempotency_store, &job.idempotency_key) {
              let _ = store.release(key).await;
          }
          let _ = queue.dead_letter(job, err.to_string()).await;
      }
      Err(err) => {
          // Only true retryable errors (Retryable, Timeout) proceed to backoff
          ...
      }
  }
  ```

---

### M5 Redis `IdempotencyStore::release` and `complete` perform unverified mutations, risking cross-worker lock deletion

- **Where:** `rume/src/jobs/redis.rs:263–309`
- **What it says:**
  ```rust
  fn complete(&self, key: &str, result: Option<Vec<u8>>) -> BoxFuture<'_, Result<(), JobError>> {
      ...
      let mut val = "COMPLETED:".to_string();
      if let Some(payload) = result {
          val.push_str(&hex_encode(&payload));
      }
      let ttl_ms = 86_400_000_u64;
      let _: () = redis::cmd("SET")
          .arg(&redis_key)
          .arg(&val)
          .arg("PX")
          .arg(ttl_ms)
          .query_async(&mut *conn)
          .await ...
  }

  fn release(&self, key: &str) -> BoxFuture<'_, Result<(), JobError>> {
      ...
      let _: () = redis::cmd("DEL")
          .arg(&redis_key)
          .query_async(&mut *conn)
          .await ...
  }
  ```
- **Why it's a problem:** In `RedisIdempotencyStore`, both `complete` and `release` perform unconditional Redis operations (`SET` and `DEL`).
  
  In distributed environments:
  1. Worker 1 acquires idempotency key `order_1` with a 300s TTL.
  2. Worker 1 suffers an extreme GC pause or processing delay exceeding 300s. The key expires in Redis.
  3. Worker 2 acquires key `order_1` and sets `IN_PROGRESS`.
  4. Worker 1 wakes up and fails, calling `store.release("order_1")`.
  5. Worker 1 executes `DEL rume:idempotency:order_1`.
  6. This unconditionally deletes Worker 2's active lock!
  7. Worker 3 can now acquire key `order_1` while Worker 2 is still executing, breaking mutual exclusion.
- **Suggested fix:**
  1. In `try_acquire`, return a unique acquisition fence token (e.g. `owner_token = Uuid::new_v4().to_string()`), storing `IN_PROGRESS:<token>` in Redis.
  2. Update `complete` and `release` to accept `owner_token`.
  3. Execute atomic Lua scripts for both operations:
     For release:
     ```lua
     if redis.call('GET', KEYS[1]) == ARGV[1] then
         return redis.call('DEL', KEYS[1])
     else
         return 0
     end
     ```
     For complete:
     ```lua
     if redis.call('GET', KEYS[1]) == ARGV[1] then
         return redis.call('SET', KEYS[1], ARGV[2], 'PX', ARGV[3])
     else
         return 0
     end
     ```

---

### M6 `PostgresJobQueue::enqueue_delayed` returns fictitious IDs on duplicate conflict resolution race

- **Where:** `rume/src/jobs/postgres.rs:120–140`
- **What it says:**
  ```rust
  if let Some(r) = row {
      let inserted_id: String = r.try_get(0).unwrap_or(id);
      Ok(inserted_id)
  } else if let Some(ref ikey) = idempotency_key {
      let existing = sqlx::query(
          "SELECT id FROM rume_jobs WHERE idempotency_key = $1 AND status != 'dead_letter'"
      )
      .bind(ikey)
      .fetch_optional(&self.pool)
      .await
      .map_err(|e| JobError::Fatal(e.to_string()))?;

      if let Some(erow) = existing {
          Ok(erow.try_get(0).unwrap_or(id))
      } else {
          Ok(id)
      }
  } else {
      Ok(id)
  }
  ```
- **Why it's a problem:** When `INSERT_JOB_SQL` encounters an idempotency key conflict (`ON CONFLICT (idempotency_key) ... DO NOTHING`), `row` is `None`. The code falls into the `else if` block and attempts to look up the conflicting existing job ID.
  
  However, if the existing job was acknowledged (`DELETE FROM rume_jobs`) or transitioned to `dead_letter` between the `INSERT` and the `SELECT`, `existing` evaluates to `None`.
  
  In this branch (line 136), `enqueue_delayed` executes `Ok(id)`, returning the freshly generated `Uuid::new_v4().to_string()`. This ID was **never inserted into the database**! The caller receives an `Ok(id)` suggesting that the job was queued, but the database contains no record of this job. Upstream tracking systems referencing this returned ID will find it nonexistent.
- **Suggested fix:**
  If `existing` is `None` after a conflict, loop back and re-attempt the `INSERT` once (since the conflicting row has been deleted/completed), or return an explicit `Err(JobError::Fatal("idempotency conflict race: existing job was removed mid-flight".into()))` instead of returning a phantom ID.

---

### m1 Wall-clock underflow panic in `PostgresJobQueue::dequeue_for` on systems with low monotonic uptime

- **Where:** `rume/src/jobs/postgres.rs:186–190`
- **What it says:**
  ```rust
  let available_at = if available_at_ms <= now {
      Instant::now() - Duration::from_millis((now - available_at_ms).max(0) as u64)
  } else {
      Instant::now() + Duration::from_millis((available_at_ms - now).max(0) as u64)
  };
  ```
- **Why it's a problem:** In the Rust standard library, `Instant::sub(Duration)` panics if the subtracted duration exceeds the monotonic clock's origin (system boot time). If a host reboots and starts processing a job enqueued hours or days prior, `(now - available_at_ms)` will be greater than the host's uptime. Invoking `Instant::now() - Duration::from_millis(...)` causes an unhandled panic that crashes the dequeue task.
- **Suggested fix:**
  For jobs whose `available_at_ms <= now`, the job is already available; simply set `available_at = Instant::now()`, or use `Instant::now().checked_sub(...).unwrap_or_else(Instant::now)`.

---

### m2 Exponential backoff in `RetryPolicy` lacks randomized jitter and is vulnerable to NaN/overflow

- **Where:** `rume/src/jobs/retry.rs:68–73`
- **What it says:**
  ```rust
  let exponent = (attempt.saturating_sub(1)) as i32;
  let factor = self.backoff_factor.powi(exponent);
  let backoff_ms = (self.initial_backoff_ms as f64 * factor).round() as u64;
  let capped_ms = backoff_ms.min(self.max_backoff_ms);
  Some(Duration::from_millis(capped_ms))
  ```
- **Why it's a problem:**
  1. **Thundering herd:** Backoff calculation is completely deterministic without jitter. When an external service or database suffers a transient outage, hundreds of failed jobs all retry at identical millisecond offsets ($1\text{s}, 2\text{s}, 4\text{s}$), creating synchronized retry storms against the recovering service.
  2. **Float safety:** If `backoff_factor` is `NaN`, `NaN.round() as u64` yields `0`, causing immediate un-throttled retry loops.
- **Suggested fix:**
  Incorporate standard full jitter (e.g. `fastrand::u64(0..=capped_ms)`) and validate that `backoff_factor.is_finite() && backoff_factor >= 1.0`.

---

### m3 `JobWorker` lacks cancellation token propagation to `JobContext` during graceful drain

- **Where:** `rume/src/jobs/job.rs:40–47` and `rume/src/jobs/worker.rs:70–75, 346–358`
- **What it says:**
  `JobContext` contains only `id`, `attempt`, and `max_attempts`.
  In `JobWorker::drain(timeout)`, the worker sets `is_stopping = true` and polls `in_flight_count`.
- **Why it's a problem:** Long-running handlers (e.g. iterating over database cursors or batch processing) have no way to observe that the worker is draining. When `drain(timeout)` times out, the application proceeds to tear down connection pools, leaving in-flight handlers to abort mid-operation with broken pipe/connection closed errors.
- **Suggested fix:**
  Add a `cancellation_token: tokio_util::sync::CancellationToken` to `JobContext` and trigger it when `JobWorker::drain` begins.

---

### m4 DLQ lacks failure timestamp and query/replay administrative APIs

- **Where:** `rume/src/jobs/queue.rs:43–50` and `rume/src/persistence/sql/postgres/jobs_schema.sql:1–18`
- **What it says:**
  `rume_jobs` stores `dead_letter_reason TEXT`, but contains no `dead_lettered_at` timestamp.
  `JobQueue` exposes only `dead_letter_count(&self)`.
- **Why it's a problem:** The dead-letter queue is an unobservable write-only sink. Platform operators cannot determine when a job was dead-lettered, query dead-lettered jobs by time or error type, or replay them after resolving bugs.
- **Suggested fix:**
  Add `dead_lettered_at BIGINT` to `rume_jobs`, and add `list_dead_letters` and `replay_dead_letter(job_id)` methods to `JobQueue`.

---

### m5 Worker poll error loop busy-spins every 50ms during database outages

- **Where:** `rume/src/jobs/worker.rs:287–290`
- **What it says:**
  ```rust
  Err(_) => {
      drop(permit);
      sleep(Duration::from_millis(50)).await;
  }
  ```
- **Why it's a problem:** If PostgreSQL goes down or exhausts its connection pool, each worker instance retries every 50ms (20 times per second), hammering the failed database with reconnect attempts and flooding logs.
- **Suggested fix:**
  Apply exponential backoff on consecutive poll errors (e.g. 50ms $\to$ 200ms $\to$ 1000ms $\to$ 5000ms cap).

---

### m6 Dual lease expiration recovery between `jobs_claim.sql` and `jobs_reap.sql` lacks coordination

- **Where:** `rume/src/persistence/sql/postgres/jobs_claim.sql:9` and `rume/src/persistence/sql/postgres/jobs_reap.sql:5–6`
- **What it says:**
  Both `jobs_claim.sql` (`OR (status = 'in_flight' AND lease_expires_at <= $3)`) and `jobs_reap.sql` (`WHERE status = 'in_flight' AND (assigned_worker = ANY($1) OR lease_expires_at <= $2)`) attempt to recover expired claims.
- **Why it's a problem:** `jobs_claim.sql` claims and increments `attempt` directly, whereas `jobs_reap.sql` resets status to `'pending'` without touching `attempt`. When a reaped job is subsequently claimed, `attempt` is incremented again. Furthermore, `jobs_reap.sql` lacks `FOR UPDATE SKIP LOCKED`, causing lock contention with `jobs_claim.sql`.
- **Suggested fix:**
  Consolidate lease expiration recovery into `jobs_claim.sql`, and restrict `jobs_reap.sql` solely to dead worker reaping (`assigned_worker = ANY($1)`), adding `FOR UPDATE SKIP LOCKED`.

---

## Positive notes

- **Row-level concurrency via `FOR UPDATE SKIP LOCKED`:** In `rume/src/persistence/sql/postgres/jobs_claim.sql:12`, using `FOR UPDATE SKIP LOCKED` guarantees that concurrent worker nodes never block each other or claim overlapping jobs, providing clean $O(1)$ lock-free dequeueing.
- **RAII-guarded in-flight accounting:** `InFlightGuard` in `rume/src/jobs/worker.rs:89–95` ensures that worker in-flight counts and concurrency permits are unconditionally decremented upon task drop or panic, fixing the graceful shutdown hang from v02.
- **Tokio semaphore concurrency limiting:** Bounding background worker tasks via `tokio::sync::Semaphore` (`rume/src/jobs/worker.rs:169, 264–278`) prevents queue spikes from spawning unbounded async tasks, protecting memory and database connection pools.
- **Execution timeout enforcement:** Wrapping handler dispatch in `tokio::time::timeout` (`rume/src/jobs/worker.rs:77–82`) ensures that hanging handlers are reliably aborted with `JobError::Timeout`.
- **Clean modular traits:** Decoupling `JobQueue`, `HeartbeatRegistry`, `IdempotencyStore`, and `JobRegistry` provides clean boundaries for testing and backend substitutability.

---

## Verdict

**Not ready for production.**

The background jobs subsystem exhibits critical state ownership and concurrency flaws that will lead to duplicate execution and data loss under production failure conditions:
1. **C1** — State mutations (`ack`, `nack`, `dead_letter`) must enforce lease tokens and worker ownership to prevent superseded workers from deleting or corrupting jobs owned by peer workers.
2. **C2** — Implement a job-level lease renewal / heartbeat API (`renew_lease`) and spawn an active renewal loop in `JobWorker` so long-running jobs are not re-driven while still executing.
3. **C3** — Retain completed jobs or store idempotency keys in a dedicated table during `ack()` so PostgreSQL-level unique index deduplication survives job completion.
4. **M1** — Wrap handler execution in `catch_unwind` to dead-letter panicked jobs and release idempotency locks instead of stranding jobs in-flight.
5. **M2** — Maintain idempotency locks during retry backoff delays rather than releasing them immediately on retryable failures.
6. **M3** — Re-queue jobs with a delay upon encountering `IdempotencyStatus::InProgress` rather than abandoning them in `in_flight` status.
7. **M4** — Route non-retryable errors (`UnknownJob`, `Serialization`) immediately to DLQ without burning retry budgets.
8. **M5** — Verify ownership tokens in Redis Lua scripts before completing or releasing idempotency keys.
9. **M6** — Prevent `PostgresJobQueue::enqueue_delayed` from returning phantom job IDs when encountering idempotency conflict resolution races.
