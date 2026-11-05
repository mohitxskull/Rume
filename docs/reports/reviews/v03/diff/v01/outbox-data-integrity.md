# Outbox & Data Integrity Diff Review — v03 (v01)

## Summary

This diff review evaluates the outbox and data integrity subsystem in Rume across commits `cf82559..HEAD`, auditing `crates/core/src/outbox/`, SQL schemas and queries, database backends, and integration tests. All three critical data-loss vulnerabilities from v03 (C1 unvalidated lease tokens in completions, C2 premature lease revocation via message age, and C3 upsert status corruption) have been fully and cleanly resolved with robust integration test verification. However, the change **fails** diff review and cannot be accepted as-is because the remediation for dispatcher panic isolation (v03 M5) is defective: `std::panic::catch_unwind` wraps only the future's construction closure rather than its execution, allowing asynchronous dispatcher panics to unwind uncaught and permanently terminate the background relay loop (Finding M1).

---

## Prior decisions

- **v03 Outbox C1 (Superseded relay worker overwrites active claims and resurrects published messages via unvalidated lease tokens in `mark_published()` and `mark_failed()`)** — **RESOLVED**.
  `OutboxRepository` signatures in `crates/core/src/outbox/repository.rs:40–50` now mandate `lease_token: &str`:
  ```rust
  fn mark_published(&self, id: &str, lease_token: &str) -> BoxFuture<'_, Result<(), OutboxError>>;
  fn mark_failed(&self, id: &str, lease_token: &str, error: String) -> BoxFuture<'_, Result<(), OutboxError>>;
  ```
  In PostgreSQL (`postgres.rs:394, 435`), MySQL (`mysql.rs:411, 452`), and SQLite (`sqlite.rs:419, 460`), the update statements enforce `WHERE id = $1 AND lease_token = $2 AND status = 'inflight'`. Crucially, all backends assert `res.rows_affected() > 0`; if zero rows are modified, `Err(OutboxError::Repository(...))` is returned, rejecting superseded workers whose lease expired or was revoked. Verified by integration test `outbox_superseded_worker_cannot_mark_published_or_failed` in `crates/core/tests/integration/outbox.rs:436–478`.

- **v03 Outbox C2 (Premature lease revocation in `outbox_recover.sql` via message age condition causes unconditional concurrent double-dispatch)** — **RESOLVED**.
  Across `crates/core/src/persistence/sql/{postgres,mysql,sqlite}/outbox_recover.sql`, the clause `OR ($1 - created_at) >= $2` was excised. The recovery query evaluates strictly against granted lease expiration:
  ```sql
  WHERE status = 'inflight'
    AND (lease_expires_at <= $1 OR lease_expires_at IS NULL);
  ```
  All three persistence drivers bind only the current timestamp `now` as `$1`. Unsigned subtraction underflow under clock skew (MySQL error 1690) is eliminated, and active leases on messages queued for $\ge 30$ seconds remain undisturbed. Verified by integration test `outbox_recovery_does_not_prematurely_revoke_valid_lease_on_old_message` in `crates/core/tests/integration/outbox.rs:481–511`.

- **v03 Outbox C3 (Duplicate message insertion with upsert resurrects already-published and in-flight messages)** — **RESOLVED**.
  `outbox_insert.sql` was replaced with `ON CONFLICT (id) DO NOTHING` in PostgreSQL (`postgres/outbox_insert.sql:5`) and SQLite (`sqlite/outbox_insert.sql:5`), and `ON DUPLICATE KEY UPDATE id = id` in MySQL (`mysql/outbox_insert.sql:5`). Re-inserting an existing message ID (such as during client HTTP idempotency retries or saga step replay) is a no-op that preserves existing publication status, retry counts, and lease locks. Verified by integration test `outbox_duplicate_insert_preserves_published_and_inflight_state` in `crates/core/tests/integration/outbox.rs:514–539`.

- **v03 Outbox M1 (Missing `ORDER BY created_at ASC` in MySQL `outbox_claim.sql` violates FIFO temporal event ordering)** — **RESOLVED**.
  `crates/core/src/persistence/sql/mysql/outbox_claim.sql:6` now includes `ORDER BY created_at ASC, id ASC` before `LIMIT ?`. (The clause was also mirrored in PostgreSQL and SQLite with secondary sorting on `id ASC` to break ties). FIFO causal ordering is strictly guaranteed across all database engines.

- **v03 Outbox M2 (Relay error propagation swallows recovery failures and abandons claimed in-flight messages on partial batch failures)** — **RESOLVED**.
  In `crates/core/src/outbox/relay.rs:91–98`, recovery errors are explicitly logged via `tracing::warn!(error = %err, "Failed to recover expired outbox claims")` instead of silently swallowed with `let _ = ...`. In the dispatch loop (`relay.rs:122–144`), errors in `mark_published` and `mark_failed` are logged with `tracing::error!` and do not early-return out of the batch; subsequent claimed messages continue to be processed and dispatched, preventing batch stranding.

- **v03 Outbox M3 (Non-atomic read-modify-write race condition in `mark_failed()` overwrites concurrent retry counters)** — **RESOLVED**.
  Both `SELECT retry_count` and `UPDATE rume_outbox` in `postgres.rs:416–446`, `mysql.rs:433–463`, and `sqlite.rs:441–471` now enforce `WHERE id = $1 AND lease_token = $2 AND status = 'inflight'`. Because lease tokens are cryptographically random UUIDs uniquely generated per claim, concurrent workers can never hold the same token. If a worker's lease expires and another worker claims the row, any subsequent update by the superseded worker fails with zero rows affected. The lost-update race condition identified in M3 is neutralized.

- **v03 Outbox M4 (Outbox repository lacks dead-letter queue (DLQ) query/reprocess API and visibility)** — **RESOLVED**.
  `OutboxRepository` trait declares `failed_count()`, `fetch_failed(limit)`, and `reprocess_failed(id)`. Fully implemented with parameterized SQL in `PostgresBackend` (`postgres.rs:498–567`), `MysqlBackend` (`mysql.rs:515–584`), `SqliteBackend` (`sqlite.rs:523–592`), and delegated through `DatabasePool<B>` with pool admission permits (`pool.rs:437–469`). Verified by integration test `outbox_relay_tracks_failures_and_exhaustion` in `crates/core/tests/integration/outbox.rs:83–144`.

- **v03 Outbox M5 (Unhandled dispatcher panics permanently terminate `OutboxRelay::start()` background worker loop)** — **CHALLENGED (Defective Remediation)**.
  The fix in `relay.rs:113–120` wrapped `(self.dispatcher)(&message)` in `std::panic::catch_unwind(...)`. However, `self.dispatcher` returns a pinned future (`Pin<Box<dyn Future>>`), and creating the future does not execute the async code. The async body executes only when `fut.await` is evaluated outside `catch_unwind`. An async panic inside user dispatcher code still unwinds through `poll_and_dispatch()` and kills the spawned Tokio task in `start()`. Re-reported as finding M1.

- **v03 Outbox m1 (Hardcoded retry thresholds, lease durations, and batch sizes preclude workload tuning)** — **RESOLVED (Partially)**.
  `OutboxRelayConfig` was introduced in `relay.rs:20–37` with `batch_size`, `lease_duration_secs`, and `poll_interval`, and exposed on `OutboxRelay::with_config` and `start_with_config`. However, the max retries threshold remains hardcoded to `5` in all database drivers (`postgres.rs:430`, `mysql.rs:447`, `sqlite.rs:455`). Re-reported as finding m1.

- **v03 Outbox m2 (Missing index on `(status, lease_expires_at)` causes full table scans during recovery polling)** — **RESOLVED**.
  `CREATE INDEX IF NOT EXISTS idx_rume_outbox_lease ON rume_outbox (status, lease_expires_at)` was added to `postgres/outbox_schema.sql:18` (partial index `WHERE status = 'inflight'`), `mysql/outbox_schema.sql:13`, and `sqlite/outbox_schema.sql:17`.

- **v03 Outbox m3 (1-second timestamp resolution creates non-deterministic causal ordering for high-throughput batches)** — **RESOLVED**.
  `ORDER BY created_at ASC, id ASC` was added to `outbox_claim.sql` across PostgreSQL, MySQL, and SQLite. Even when hundreds of messages are inserted within the same second, the secondary sort on `id ASC` enforces deterministic tie-breaking.

- **v03 Outbox m4 (Outbox operations on `DatabasePool<B>` bypass pool semaphores and in-flight accounting)** — **RESOLVED**.
  In `crates/core/src/persistence/pool.rs:334–469`, all 10 `OutboxRepository` methods now call `let _lease = self.acquire_lease().map_err(|e| OutboxError::Repository(e.to_string()))?;` before delegating to `self.backend()`. Outbox background operations are now properly bound by pool admission limits and connection metrics.

---

## Findings

### M1 Unhandled asynchronous dispatcher panics bypass `catch_unwind` and terminate `OutboxRelay` background loop

- **Where:** `crates/core/src/outbox/relay.rs:113–120`
- **What it says:**
  ```rust
  let dispatch_result = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
      (self.dispatcher)(&message)
  })) {
      Ok(fut) => fut.await,
      Err(_) => Err(OutboxError::Dispatch("outbox dispatcher panicked".into())),
  };
  ```
- **Why it's a problem:**
  `self.dispatcher` is defined in `relay.rs:15–17` as:
  ```rust
  pub type MessageDispatcher = Arc<
      dyn Fn(&OutboxMessage) -> BoxFuture<'static, Result<(), OutboxError>> + Send + Sync + 'static,
  >;
  ```
  In Rust's async execution model, invoking an async function or closure does **not** execute the asynchronous code; it merely constructs and returns a `Future` state machine (here boxed as `BoxFuture`).
  
  Wrapping `std::panic::catch_unwind` synchronously around `(self.dispatcher)(&message)` intercepts panics only if they occur during the construction of the future. The actual body of the dispatcher executes when `fut.await` is evaluated. Because `fut.await` is located in the match arm *outside* of `catch_unwind`, any panic occurring inside user async code (e.g., calling `.unwrap()` on invalid JSON payload, schema assertion failure, or downstream client panic) unwinds completely unshielded through `poll_and_dispatch()`.
  
  In `OutboxRelay::start()` (`relay.rs:170–178`) and `start_with_config()` (`relay.rs:152–163`), Tokio catches unwinding panics at the root of the spawned task and terminates the task immediately. The process remains alive, but the outbox relay background loop is permanently dead. No further outbox messages are dispatched until the entire application is restarted.
  
  Furthermore, because the panic unwinds out of `poll_and_dispatch()`, the panicked message is never marked failed via `mark_failed()`. It remains stuck in `InFlight` status with an active lease until `recover_expired_claims()` reclaims it, and any remaining messages in the claimed batch are abandoned until their leases expire as well.
  
  *(Note for `jobs-worker-concurrency` reviewer: `crates/core/src/jobs/worker.rs:366–371` contains the identical anti-pattern on job dispatch).*
- **Suggested fix:**
  Catch unwinding panics during the polling/awaiting of the future itself. Either add `futures-util` to `crates/core/Cargo.toml` (`futures-util = { workspace = true }`) and invoke `FutureExt::catch_unwind`:
  ```rust
  use futures_util::FutureExt;

  let dispatch_result = match std::panic::AssertUnwindSafe((self.dispatcher)(&message))
      .catch_unwind()
      .await
  {
      Ok(res) => res,
      Err(_) => Err(OutboxError::Dispatch("outbox dispatcher panicked".into())),
  };
  ```
  Or execute the dispatcher future inside a spawned Tokio task (`tokio::spawn`) and check `join_err.is_panic()`.

---

### m1 Hardcoded `max_retries` threshold of 5 remains unconfigurable across database drivers

- **Where:** `crates/core/src/persistence/postgres.rs:430`, `crates/core/src/persistence/mysql.rs:447`, `crates/core/src/persistence/sqlite.rs:455`
- **What it says:**
  ```rust
  let retry_count: i32 = row.try_get(0).unwrap_or(0) + 1;
  let status = if retry_count >= 5 { "failed" } else { "pending" };
  ```
- **Why it's a problem:**
  While `OutboxRelayConfig` parameterized `batch_size`, `lease_duration_secs`, and `poll_interval`, the threshold for dead-letter queue routing (`status = 'failed'`) remains hardcoded to `5` across all three database backends. Workloads requiring fast failure (e.g. 2 retries for latency-sensitive event streams) or extended retry endurance (e.g. 10 retries with exponential backoff for slow downstream brokers) cannot configure this threshold without modifying framework code.
- **Suggested fix:**
  Add an optional `max_retries: u32` field to `DatabaseConfig` (or pass it through `OutboxRelayConfig` to `mark_failed`), defaulting to `5` if unspecified.

---

## Positive notes

- **Strict lease fencing (`WHERE lease_token = $2 AND status = 'inflight'`)**: The inclusion of `lease_token: &str` in `mark_published` and `mark_failed` with `rows_affected() > 0` validation provides ironclad fencing against superseded workers, preventing ghost deliveries and resurrection of completed events.
- **Clean removal of message-age recovery**: Purging `($1 - created_at) >= $2` from `outbox_recover.sql` across all three database engines restores correct lease semantics, guaranteeing that messages queued under load are not prematurely reclaimed.
- **Immutable event insertion (`DO NOTHING`)**: Replacing upsert queries with `ON CONFLICT (id) DO NOTHING` in PostgreSQL/SQLite and `ON DUPLICATE KEY UPDATE id = id` in MySQL guarantees outbox row immutability across idempotent retries.
- **First-class DLQ administration**: The addition of `failed_count()`, `fetch_failed()`, and `reprocess_failed()` provides full visibility and operational recovery for poison-pill events across all backends.
- **Comprehensive pool admission gating**: Delegating all `OutboxRepository` operations through `DatabasePool::acquire_lease()` binds outbox traffic to pool semaphore limits and in-flight tracking, preventing outbox background loops from exhausting database connections.
- **Deterministic ordering**: Secondary sorting on `ORDER BY created_at ASC, id ASC` across all engines eliminates tie-breaking jitter for events generated within the same epoch second.
- **Exemplary integration test suite**: `crates/core/tests/integration/outbox.rs` contains 13 robust, deterministically synchronized tests covering all edge cases, from lease recovery to transactional rollback.

---

## Verdict

**Fail** (Must land fix pass first).

The outbox subsystem has resolved all three Critical data integrity defects (C1, C2, C3) and five of the prior findings. However, the remediation for finding M5 is defective: asynchronous dispatcher panics still bypass `catch_unwind`, causing the background relay task to terminate permanently on the first unhandled error in a message dispatcher. A fix pass resolving **Finding M1** must land before this diff can be merged to production.
