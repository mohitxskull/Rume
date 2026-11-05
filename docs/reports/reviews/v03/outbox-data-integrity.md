# Outbox & Data Integrity Review — v03

## Summary

The Rume transactional outbox subsystem provides an event-publishing architecture designed to guarantee at-least-once message delivery via database transactions, background relays, and row-level lease claiming. However, the current implementation is **not ready for production** due to severe data integrity and concurrency flaws: `mark_published()` and `mark_failed()` fail to validate worker lease ownership, allowing superseded relays to overwrite newer claims or resurrect published messages; `outbox_recover.sql` prematurely revokes active leases based on total message age, causing immediate duplicate message dispatch; and upsert queries in `outbox_insert.sql` overwrite existing records on duplicate primary keys, resetting published events back to pending. Additionally, MySQL omits FIFO `ORDER BY` ordering during claiming, partial batch relay failures strand claimed messages in-flight, and dispatcher panics permanently crash the background relay loop.

---

## Prior decisions

- **v02 Enterprise-Reliability C2 (Concurrent `OutboxRelay` instances lack claiming locks in HA deployments)** — **CHALLENGED (Defective Remediation)**. Commit `8d805cb` and Phase 5 introduced `claim_pending_with_lease` and `OutboxStatus::InFlight` using `FOR UPDATE SKIP LOCKED`. However, the implementation is incomplete and introduces new critical failure modes:
  1. `mark_published()` and `mark_failed()` accept only `id: &str` without checking `lease_token` or `status = 'inflight'`, permitting superseded workers whose lease expired to overwrite active leases or resurrect already-published messages (finding C1).
  2. `outbox_recover.sql` revokes leases based on total message creation age (`created_at`) rather than lease expiration, immediately revoking active exclusive leases and causing guaranteed double-dispatch under queue backlogs (finding C2).
  3. MySQL's `outbox_claim.sql` omits `ORDER BY created_at ASC`, breaking temporal ordering in HA deployments (finding M1).
- **v02 Enterprise-Reliability m2 (`OutboxRelay` lacks retry backoff)** — **REAFFIRMED**. Exponential backoff (`next_retry_at = now + 2^(retry-1)`) was introduced in persistence backends and enforced in `outbox_claim.sql`. However, the retry limit is hardcoded to 5 across all database engines without configuration hooks, and `mark_failed()` suffers from a read-modify-write race condition (findings M3, m1).
- **v02 Enterprise-Reliability m3 (`MemoryOutboxRepository::fetch_pending` dispatches messages in random hash order)** — **CHALLENGED (Superseded & Regressed)**. `MemoryOutboxRepository` was removed entirely in commit `53fec5f` in favor of real database persistence (`SqliteBackend::in_memory()`, `PostgresBackend`, `MysqlBackend`). While SQLite and PostgreSQL enforce `ORDER BY created_at ASC`, MySQL's `outbox_claim.sql` completely omitted `ORDER BY created_at ASC`, re-introducing non-deterministic, out-of-order dispatch specifically on MySQL (finding M1).
- **Transactional Outbox Enlistment (`TransactionalOutbox::save_tx`)** — **NOTE-NEW**. New capability introduced in Phase 5 allowing outbox records to commit or rollback atomically alongside application mutations in PostgreSQL, MySQL, and SQLite. Audited in Positive Notes and finding C3.

---

## Findings

### C1 Superseded relay worker overwrites active claims and resurrects published messages via unvalidated lease tokens in `mark_published()` and `mark_failed()`

- **Where:** `crates/core/src/outbox/repository.rs:39–44`, `crates/core/src/persistence/postgres.rs:353–395`, `crates/core/src/persistence/mysql.rs:370–413`, `crates/core/src/persistence/sqlite.rs:384–427`
- **What it says:**
  In `crates/core/src/outbox/repository.rs`:
  ```rust
  /// Marks a message as successfully published.
  fn mark_published(&self, id: &str) -> BoxFuture<'_, Result<(), OutboxError>>;

  /// Marks a message as failed with an error message and increments its retry counter.
  fn mark_failed(&self, id: &str, error: String) -> BoxFuture<'_, Result<(), OutboxError>>;
  ```
  In `crates/core/src/persistence/postgres.rs` (and identically in `mysql.rs` and `sqlite.rs`):
  ```rust
  fn mark_published(&self, id: &str) -> OutboxFuture<'_, Result<(), OutboxError>> {
      let id_str = id.to_string();
      Box::pin(async move {
          sqlx::query(
              "UPDATE rume_outbox SET status = 'published', lease_token = NULL, lease_expires_at = NULL WHERE id = $1"
          )
          .bind(&id_str)
          .execute(&self.pool)
          .await
          .map_err(|e| OutboxError::Repository(e.to_string()))?;
          Ok(())
      })
  }

  fn mark_failed(&self, id: &str, error: String) -> OutboxFuture<'_, Result<(), OutboxError>> {
      let id_str = id.to_string();
      Box::pin(async move {
          ...
          sqlx::query(
              "UPDATE rume_outbox SET status = $1, retry_count = $2, last_error = $3, next_retry_at = $4, lease_token = NULL, lease_expires_at = NULL WHERE id = $5"
          )
          ...
          .bind(&id_str)
          .execute(&self.pool)
          .await
          .map_err(|e| OutboxError::Repository(e.to_string()))?;
          Ok(())
      })
  }
  ```
- **Why it's a problem:** Neither the trait signature nor the persistence SQL queries take or validate the `lease_token`, nor do they check `WHERE status = 'inflight'`. This causes message corruption and duplicate publishing through two concrete failure mechanisms:
  1. **Relay A times out / stalls; Relay B reclaims and publishes:**
     - Relay A claims message M (`lease_token = "tok-A"`, 30s lease).
     - Downstream broker publish takes 35s (slow network or downstream broker throttle).
     - At $t=30$s, Relay A's lease expires.
     - Relay B recovers the claim and claims message M with `lease_token = "tok-B"`. Relay B publishes to the broker and calls `mark_published("M")`. Message M is now `status = 'published'`.
     - At $t=35$s, Relay A's dispatch attempt fails with a timeout error. Relay A calls `mark_failed("M", "timeout")`.
     - Relay A's update runs: `UPDATE rume_outbox SET status = 'pending', retry_count = 1 ... WHERE id = 'M'`.
     - Relay A unconditionally **overwrites** the row, resetting an already-published message back to `'pending'`!
     - In the next poll cycle, Relay C claims message M and publishes it a second time. **Double delivery occurs.**
  2. **Relay A succeeds after lease loss; wipes Relay B's active claim:**
     - If Relay A succeeds at $t=35$s and calls `mark_published("M")`, its update sets `lease_token = NULL` while Relay B is in the middle of dispatching with active lease `"tok-B"`.
  3. **Silent failure on non-existent or stale messages:**
     - The updates ignore `rows_affected()`. If a message does not exist or has already been completed, `mark_published` returns `Ok(())` silently.
- **Suggested fix:**
  1. Update `OutboxRepository` signatures to accept `lease_token: &str`:
     ```rust
     fn mark_published(&self, id: &str, lease_token: &str) -> BoxFuture<'_, Result<(), OutboxError>>;
     fn mark_failed(&self, id: &str, lease_token: &str, error: String) -> BoxFuture<'_, Result<(), OutboxError>>;
     ```
  2. Enforce lease token matching and status verification in SQL:
     ```sql
     UPDATE rume_outbox
     SET status = 'published', lease_token = NULL, lease_expires_at = NULL
     WHERE id = $1 AND lease_token = $2 AND status = 'inflight';
     ```
  3. Assert `rows_affected() == 1`. If zero rows are modified, return `Err(OutboxError::Repository("lease lost or expired"))` so the superseded caller knows its claim was invalidated.

---

### C2 Premature lease revocation in `outbox_recover.sql` via message age condition causes unconditional concurrent double-dispatch

- **Where:** `crates/core/src/persistence/sql/postgres/outbox_recover.sql:1–6`, `crates/core/src/persistence/sql/mysql/outbox_recover.sql:1–6`, `crates/core/src/persistence/sql/sqlite/outbox_recover.sql:1–6`, `crates/core/src/outbox/relay.rs:60–63`
- **What it says:**
  In `crates/core/src/persistence/sql/postgres/outbox_recover.sql` (and identically in MySQL and SQLite):
  ```sql
  UPDATE rume_outbox
  SET status = 'pending',
      lease_token = NULL,
      lease_expires_at = NULL
  WHERE status = 'inflight'
    AND (lease_expires_at <= $1 OR ($1 - created_at) >= $2);
  ```
  In `crates/core/src/outbox/relay.rs`:
  ```rust
  pub async fn poll_and_dispatch(&self, batch_size: usize) -> Result<usize, OutboxError> {
      // Automatically reclaim any expired in-flight message leases before polling new items
      let _ = self.repository.recover_expired_claims(30).await;

      let pending = self.repository.claim_pending(batch_size).await?;
  ```
- **Why it's a problem:**
  1. **Message creation age ($\text{created\_at}$) violates active leases:**
     - `$1` is bound to current timestamp `now`; `$2` is bound to `max_lease_age_secs` (`30`).
     - `created_at` records the epoch timestamp when the message was originally inserted into the outbox table.
     - Suppose message M was created at $t=1000$. During high queue load or downstream rate limits, message M waits in `pending` status for 40 seconds.
     - At $t=1040$, Relay 1 calls `claim_pending_with_lease(10, 60)`. It claims message M, setting `status = 'inflight'`, `lease_expires_at = 1100` (valid for 60 seconds).
     - At $t=1041$ (1 second later), Relay 2 executes `poll_and_dispatch()`.
     - Relay 2 calls `recover_expired_claims(30)`. The SQL evaluates:
       - `lease_expires_at <= 1041` $\rightarrow$ `1100 <= 1041` (FALSE).
       - `(1041 - created_at) >= 30` $\rightarrow$ `(1041 - 1000) >= 30` $\rightarrow$ `41 >= 30` (**TRUE**).
     - Message M's lease is **prematurely revoked** while Relay 1 is actively processing it with 59 seconds of lease remaining!
     - Message M is reset to `status = 'pending'`, and Relay 2 immediately claims it in the next line of `poll_and_dispatch()`.
     - Both Relay 1 and Relay 2 dispatch message M concurrently. **Guaranteed duplicate message publication occurs for every message that spent $\ge 30$ seconds in the queue.**
  2. **MySQL unsigned subtraction underflow crash:**
     - In MySQL (`outbox_schema.sql:7`), `created_at` is defined as `BIGINT UNSIGNED NOT NULL`.
     - In `outbox_recover.sql`, the expression `(? - created_at)` calculates `now - created_at`.
     - If the system clock experiences backwards NTP drift such that `now < created_at`, MySQL with default `NO_UNSIGNED_SUBTRACTION` mode aborts with error 1690: `BIGINT UNSIGNED value is out of range in '(? - created_at)'`, crashing the recovery query entirely.
- **Suggested fix:**
  Remove the `created_at` clause from `outbox_recover.sql` across all backends. Lease expiration must be determined strictly by whether the granted lease has expired:
  ```sql
  UPDATE rume_outbox
  SET status = 'pending',
      lease_token = NULL,
      lease_expires_at = NULL
  WHERE status = 'inflight'
    AND (lease_expires_at <= $1 OR lease_expires_at IS NULL);
  ```

---

### C3 Duplicate message insertion with upsert resurrects already-published and in-flight messages

- **Where:** `crates/core/src/persistence/sql/postgres/outbox_insert.sql:1–11`, `crates/core/src/persistence/sql/mysql/outbox_insert.sql:1–11`, `crates/core/src/persistence/sql/sqlite/outbox_insert.sql:1–11`
- **What it says:**
  In `crates/core/src/persistence/sql/postgres/outbox_insert.sql`:
  ```sql
  INSERT INTO rume_outbox (
      id, event_type, payload, status, retry_count, created_at,
      last_error, next_retry_at, lease_token, lease_expires_at
  ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
  ON CONFLICT (id) DO UPDATE SET
      status = EXCLUDED.status,
      retry_count = EXCLUDED.retry_count,
      last_error = EXCLUDED.last_error,
      next_retry_at = EXCLUDED.next_retry_at,
      lease_token = EXCLUDED.lease_token,
      lease_expires_at = EXCLUDED.lease_expires_at;
  ```
  (and identically in MySQL `ON DUPLICATE KEY UPDATE` and SQLite `ON CONFLICT(id) DO UPDATE SET`).
- **Why it's a problem:**
  1. In distributed transactional architectures, business operations and API requests are frequently retried idempotently (e.g., HTTP request retry on network drop, saga step replay). Applications generate deterministic event IDs based on business entity keys (e.g. `order-paid-{order_id}`).
  2. On the initial execution, transaction commits; message `order-paid-101` is written with `status = 'pending'`. The outbox relay claims it, publishes to Kafka/event broker, and marks it `published`.
  3. A network glitch causes the client to retry the request. The business transaction re-executes `save_tx(&tx, msg)`.
  4. The upsert triggers: `ON CONFLICT (id) DO UPDATE SET status = EXCLUDED.status`. Because the new `OutboxMessage` has status `Pending`, the database **overwrites** the existing `published` record, resetting `status` back to `'pending'`, clearing `retry_count`, and clearing error details.
  5. The outbox relay picks up `order-paid-101` again and publishes a **duplicate event** to the broker.
  6. If the message was currently `InFlight`, the upsert destroys the active worker's lease token mid-execution, resetting it to `pending` and enabling immediate duplicate claiming.
- **Suggested fix:**
  Outbox records represent immutable log entries. An insert with an existing message ID must preserve the existing record. Update the SQL across all backends to `DO NOTHING`:
  - PostgreSQL: `ON CONFLICT (id) DO NOTHING;`
  - SQLite: `ON CONFLICT (id) DO NOTHING;`
  - MySQL: `ON DUPLICATE KEY UPDATE id = id;` (or `INSERT IGNORE INTO rume_outbox ...`).
  If a business transaction re-inserts an already-persisted outbox ID, the operation is a no-op that leaves the existing publication state intact.

---

### M1 Missing `ORDER BY created_at ASC` in MySQL `outbox_claim.sql` violates FIFO temporal event ordering

- **Where:** `crates/core/src/persistence/sql/mysql/outbox_claim.sql:1–7`
- **What it says:**
  In `crates/core/src/persistence/sql/mysql/outbox_claim.sql`:
  ```sql
  SELECT id, event_type, payload, status, retry_count,
         created_at, last_error, next_retry_at, lease_token, lease_expires_at
  FROM rume_outbox
  WHERE status = 'pending'
    AND (next_retry_at IS NULL OR next_retry_at <= ?)
  LIMIT ?
  FOR UPDATE SKIP LOCKED;
  ```
  In PostgreSQL (`postgres/outbox_claim.sql:6`) and SQLite (`sqlite/outbox_claim.sql:6`):
  ```sql
  ORDER BY created_at ASC
  ```
- **Why it's a problem:** In PostgreSQL and SQLite, claims are ordered strictly by `created_at ASC`, ensuring FIFO event dispatch. In MySQL, `ORDER BY created_at ASC` was omitted. MySQL returns matching rows in primary key or physical page order. Under continuous event ingestion, older events can starve indefinitely while newer events are claimed, directly violating causal and temporal event delivery guarantees for domain subscribers.
- **Suggested fix:** Add `ORDER BY created_at ASC` before `LIMIT ?` in `crates/core/src/persistence/sql/mysql/outbox_claim.sql`:
  ```sql
  SELECT id, event_type, payload, status, retry_count,
         created_at, last_error, next_retry_at, lease_token, lease_expires_at
  FROM rume_outbox
  WHERE status = 'pending'
    AND (next_retry_at IS NULL OR next_retry_at <= ?)
  ORDER BY created_at ASC
  LIMIT ?
  FOR UPDATE SKIP LOCKED;
  ```

---

### M2 Relay error propagation swallows recovery failures and abandons claimed in-flight messages on partial batch failures

- **Where:** `crates/core/src/outbox/relay.rs:60–78`
- **What it says:**
  ```rust
  pub async fn poll_and_dispatch(&self, batch_size: usize) -> Result<usize, OutboxError> {
      // Automatically reclaim any expired in-flight message leases before polling new items
      let _ = self.repository.recover_expired_claims(30).await;

      let pending = self.repository.claim_pending(batch_size).await?;
      let mut published_count = 0;

      for message in pending {
          match (self.dispatcher)(&message).await {
              Ok(()) => {
                  self.repository.mark_published(&message.id).await?;
                  published_count += 1;
              }
              Err(err) => {
                  self.repository
                      .mark_failed(&message.id, err.to_string())
                      .await?;
              }
          }
      }

      Ok(published_count)
  }
  ```
- **Why it's a problem:**
  1. `let _ = self.repository.recover_expired_claims(30).await;` silently swallows repository errors. If database connectivity is degraded, permissions fail, or queries lock, expired in-flight claims remain stranded indefinitely with zero error logging or metrics alerts.
  2. In the dispatch loop, `self.repository.mark_published(...).await?` and `self.repository.mark_failed(...).await?` use `?`. If updating message 2 of a 50-message claimed batch fails with a transient database error, `poll_and_dispatch` immediately exits.
  3. Messages 3 through 50 were already claimed and transitioned to `InFlight` in the database. Because the function aborted early, these messages are never dispatched in this cycle. They remain locked in `InFlight` until their leases expire 30 seconds later, inflating dispatch latency by 30 seconds.
- **Suggested fix:**
  1. Log any error returned by `recover_expired_claims`:
     ```rust
     if let Err(err) = self.repository.recover_expired_claims(30).await {
         tracing::error!(error = %err, "Failed to recover expired outbox claims");
     }
     ```
  2. Handle errors during status updates without aborting remaining messages in the batch, or collect errors and return partial success counts while attempting to process the remaining items.

---

### M3 Non-atomic read-modify-write race condition in `mark_failed()` overwrites concurrent retry counters

- **Where:** `crates/core/src/persistence/postgres.rs:370–390`, `crates/core/src/persistence/mysql.rs:387–408`, `crates/core/src/persistence/sqlite.rs:401–422`
- **What it says:**
  ```rust
  let row = sqlx::query("SELECT retry_count FROM rume_outbox WHERE id = $1")
      .bind(&id_str)
      .fetch_optional(&self.pool)
      .await
      .map_err(|e| OutboxError::Repository(e.to_string()))?;

  let retry_count: i32 = row.and_then(|r| r.try_get(0).ok()).unwrap_or(0) + 1;
  let status = if retry_count >= 5 { "failed" } else { "pending" };
  let backoff_secs = 2u64.pow(retry_count.saturating_sub(1) as u32);
  let next_retry = now + backoff_secs;

  sqlx::query(
      "UPDATE rume_outbox SET status = $1, retry_count = $2, last_error = $3, next_retry_at = $4, lease_token = NULL, lease_expires_at = NULL WHERE id = $5"
  )
  ```
- **Why it's a problem:** `mark_failed()` performs an un-isolated `SELECT retry_count` followed by an `UPDATE`. If two workers process attempts or handle failure recovery concurrently for the same message, both execute `SELECT` concurrently, read the same `retry_count`, and both update the row with identical incremented values ($N+1$ instead of $N+2$). This lost update delays or prevents poison pills from reaching the threshold of 5, amplifying retry storms during downstream broker outages.
- **Suggested fix:** Perform the retry count increment and status transition atomically in SQL within a single query:
  ```sql
  UPDATE rume_outbox
  SET retry_count = retry_count + 1,
      status = CASE WHEN retry_count + 1 >= 5 THEN 'failed' ELSE 'pending' END,
      last_error = $1,
      next_retry_at = $2,
      lease_token = NULL,
      lease_expires_at = NULL
  WHERE id = $3 AND lease_token = $4 AND status = 'inflight';
  ```

---

### M4 Outbox repository lacks dead-letter queue (DLQ) query/reprocess API and visibility

- **Where:** `crates/core/src/outbox/repository.rs:15–60`, `crates/core/src/persistence/postgres.rs:412–424`, `crates/core/src/persistence/mysql.rs:430–442`, `crates/core/src/persistence/sqlite.rs:443–455`
- **What it says:**
  `OutboxRepository` defines `pending_count()` and `published_count()`, but no `failed_count()`, `fetch_failed()`, or `reprocess_failed()`.
  In `pending_count()`:
  ```rust
  let row = sqlx::query(
      "SELECT COUNT(*) FROM rume_outbox WHERE status = 'pending' OR status = 'inflight'"
  )
  ```
- **Why it's a problem:** When an outbox message exhausts 5 delivery attempts, its status becomes `'failed'`. However, `pending_count()` explicitly excludes `'failed'` messages. There is no method on `OutboxRepository` or `OutboxRelay` to count, list, or requeue dead-lettered messages. In production, poison pills accumulate silently in the database; queue monitoring dashboard alerts based on `pending_count()` report 0 pending messages while thousands of events have failed permanently without notification.
- **Suggested fix:**
  1. Add dead-letter queue methods to `OutboxRepository`:
     ```rust
     fn failed_count(&self) -> BoxFuture<'_, Result<usize, OutboxError>>;
     fn fetch_failed(&self, limit: usize) -> BoxFuture<'_, Result<Vec<OutboxMessage>, OutboxError>>;
     fn reprocess_failed(&self, id: &str) -> BoxFuture<'_, Result<(), OutboxError>>;
     ```
  2. Implement these methods across PostgreSQL, MySQL, and SQLite.

---

### M5 Unhandled dispatcher panics permanently terminate `OutboxRelay::start()` background worker loop

- **Where:** `crates/core/src/outbox/relay.rs:84–94`
- **What it says:**
  ```rust
  pub fn start(&self, interval: Duration) {
      let relay = self.clone();
      tokio::spawn(async move {
          while !relay.is_stopping.load(Ordering::Relaxed) {
              if let Err(err) = relay.poll_and_dispatch(50).await {
                  tracing::error!(error = %err, "Outbox relay background dispatch error");
              }
              sleep(interval).await;
          }
      });
  }
  ```
- **Why it's a problem:** The background task spawned in `start()` does not catch unwinding panics. If the user-supplied `dispatcher` closure panics (e.g. unwrap on malformed payload, schema assertion failure, network client panic), the panic unwinds directly through `poll_and_dispatch()`. Tokio terminates the spawned task immediately. The process remains alive, but the outbox relay background loop is permanently dead. No further outbox messages are dispatched until the entire process is restarted.
- **Suggested fix:** Catch panics at the dispatch boundary using `std::panic::AssertUnwindSafe` and `futures::FutureExt::catch_unwind`:
  ```rust
  let dispatch_res = std::panic::AssertUnwindSafe((self.dispatcher)(&message))
      .catch_unwind()
      .await;
  match dispatch_res {
      Ok(Ok(())) => { ... }
      Ok(Err(err)) => { ... }
      Err(_panic) => {
          tracing::error!(message_id = %message.id, "Outbox dispatcher panicked during message execution");
          self.repository.mark_failed(&message.id, token, "dispatcher panic".into()).await?;
      }
  }
  ```

---

### m1 Hardcoded retry thresholds, lease durations, and batch sizes preclude workload tuning

- **Where:** `crates/core/src/outbox/relay.rs:61, 88`, `crates/core/src/outbox/repository.rs:29`, `crates/core/src/persistence/postgres.rs:378`, `crates/core/src/persistence/mysql.rs:395`, `crates/core/src/persistence/sqlite.rs:409`
- **What it says:**
  - Max retries is hardcoded to `5` in `postgres.rs:378`, `mysql.rs:395`, and `sqlite.rs:409`.
  - Lease duration is hardcoded to `30` in `repository.rs:29`.
  - Recovery age is hardcoded to `30` in `relay.rs:61`.
  - Background batch size is hardcoded to `50` in `relay.rs:88`.
- **Why it's a problem:** High-throughput streaming systems require small batch sizes and low latency, whereas bulk notification relays need larger batches and longer lease durations. Users cannot tune these parameters without modifying core framework code.
- **Suggested fix:** Introduce an `OutboxRelayConfig` struct:
  ```rust
  pub struct OutboxRelayConfig {
      pub batch_size: usize,
      pub lease_duration_secs: u64,
      pub max_retries: u32,
      pub poll_interval: Duration,
  }
  ```

---

### m2 Missing index on `(status, lease_expires_at)` causes full table scans during recovery polling

- **Where:** `crates/core/src/persistence/sql/postgres/outbox_schema.sql:14–16`, `crates/core/src/persistence/sql/mysql/outbox_schema.sql:12`, `crates/core/src/persistence/sql/sqlite/outbox_schema.sql:14–15`
- **What it says:**
  In PostgreSQL:
  ```sql
  CREATE INDEX IF NOT EXISTS idx_rume_outbox_pending
  ON rume_outbox (status, created_at)
  WHERE status = 'pending';
  ```
  In MySQL: `INDEX idx_rume_outbox_claim (status, created_at)`.
- **Why it's a problem:** On every relay loop cycle, `recover_expired_claims()` queries `WHERE status = 'inflight' AND lease_expires_at <= $1`. Because no index includes `lease_expires_at` or indexes `status = 'inflight'`, every recovery poll requires a full sequential table scan of `rume_outbox`. In high-volume production tables with millions of historical records, this causes continuous disk I/O pressure and query stalls.
- **Suggested fix:** Add an index on `(status, lease_expires_at)` (or a partial index `WHERE status = 'inflight'` on PostgreSQL).

---

### m3 1-second timestamp resolution creates non-deterministic causal ordering for high-throughput batches

- **Where:** `crates/core/src/outbox/message.rs:11–16`
- **What it says:**
  ```rust
  pub(crate) fn current_timestamp() -> u64 {
      SystemTime::now()
          .duration_since(UNIX_EPOCH)
          .unwrap_or_default()
          .as_secs()
  }
  ```
- **Why it's a problem:** `created_at` records timestamps in whole seconds (`as_secs()`). At moderate enterprise throughput (e.g. 1,000 events/sec), hundreds of messages share the identical `created_at` timestamp. Because `OUTBOX_CLAIM_SQL` orders solely by `ORDER BY created_at ASC`, the database engine returns rows within the same second in non-deterministic order, breaking causal event sequence guarantees.
- **Suggested fix:** Use millisecond resolution (`as_millis()`) for `created_at`, or append a secondary deterministic sort column: `ORDER BY created_at ASC, id ASC`.

---

### m4 Outbox operations on `DatabasePool<B>` bypass pool semaphores and in-flight accounting

- **Where:** `crates/core/src/persistence/pool.rs:311–377`
- **What it says:**
  ```rust
  impl<B> OutboxRepository for DatabasePool<B>
  where
      B: DatabaseBackend + OutboxRepository,
  {
      fn save(&self, msg: OutboxMessage) -> OutboxFuture<'_, Result<(), OutboxError>> {
          self.backend().save(msg)
      }
      fn claim_pending_with_lease(...) {
          self.backend().claim_pending_with_lease(limit, lease_duration_secs)
      }
  ...
  ```
- **Why it's a problem:** `DatabasePool` tracks lease semaphores and active counters (`active_leases`, `in_flight_transactions`). However, `DatabasePool` forwards all `OutboxRepository` operations directly to `self.backend()`, acquiring raw connections from the underlying SQLx pool and completely bypassing `DatabasePool`'s semaphore admission limits and metrics tracking.
- **Suggested fix:** Acquire pool permits through `self.acquire_lease()` before delegating to `backend()`, or document that outbox background traffic uses a dedicated unthrottled pool slice.

---

## Positive notes

- **True transactional atomicity (`save_tx`)**: `TransactionalOutbox::save_tx` correctly enlists outbox writes into active database transactions (`PostgresTransaction`, `MysqlTransaction`, `SqliteTransaction`). Mutex synchronization on `active_tx` guarantees that outbox events commit if and only if domain entity state changes commit, cleanly rolling back on abort without leaving orphaned messages.
- **Concurrency isolation via `FOR UPDATE SKIP LOCKED`**: In PostgreSQL and MySQL, `claim_pending_with_lease` leverages database engine row locking (`FOR UPDATE SKIP LOCKED`), eliminating lock contention between concurrent relays and ensuring disjoint message batch claiming.
- **Clean typed event serialization ergonomics**: `OutboxMessage::from_event` and `deserialize_payload` provide seamless serialization of typed events implementing `Event + Serialize + DeserializeOwned`, eliminating manual JSON string manipulation in business code.
- **Graceful shutdown drain contracts (`try_drain` / `drain`)**: `OutboxRelay::try_drain` verifies that pending message counts reach zero before allowing shutdown to complete, returning structured error diagnostics on repository failures.

---

## Verdict

**Not ready for production.**

The transactional outbox implementation is structurally sound in its transaction enlistment (`save_tx`) and row-locking claim semantics (`FOR UPDATE SKIP LOCKED`), but contains **3 Critical findings** and **5 Major findings** that will cause duplicate message delivery, active lease invalidation, and background relay freezes under enterprise workloads:

1. **C1** — Enforce lease token ownership in `mark_published(id, lease_token)` and `mark_failed(id, lease_token)` (`WHERE id = $1 AND lease_token = $2 AND status = 'inflight'`) to prevent superseded relays from overwriting active claims or resurrecting published events.
2. **C2** — Remove `($1 - created_at) >= $2` from `outbox_recover.sql` across all backends to prevent premature lease revocation of queued messages.
3. **C3** — Replace `ON CONFLICT (id) DO UPDATE` with `ON CONFLICT (id) DO NOTHING` in `outbox_insert.sql` across all backends to guarantee event immutability on idempotent retries.
4. **M1** — Add `ORDER BY created_at ASC` to MySQL `outbox_claim.sql` to restore FIFO event ordering.
5. **M2** — Propagate recovery errors and isolate per-message dispatch errors in `poll_and_dispatch` to prevent stranding claimed batches.
6. **M3** — Atomicize `retry_count` increments in `mark_failed` SQL to eliminate read-modify-write lost updates.
7. **M4** — Expose Dead-Letter Queue APIs (`failed_count`, `fetch_failed`, `reprocess_failed`) on `OutboxRepository`.
8. **M5** — Wrap dispatcher invocations in `catch_unwind` to prevent task panics from killing the background relay loop.
