# Task 04 — Outbox & Jobs SQL / Locking Hardening (`RC-03`)

## Goal

Eliminate SQLite outbox read-to-write lock upgrade (`SQLITE_BUSY`) and `.fetch_one()` `RowNotFound` errors, decouple non-claiming `fetch_pending` queries from `FOR UPDATE SKIP LOCKED` in Postgres/MySQL outbox, and restore `trace_context` selection and strict decoding in `PostgresJobQueue::list_dead_letters`.

## Files touched

- `crates/messaging/src/outbox/sqlite.rs` (`claim_pending_with_lease`)
- `crates/messaging/src/outbox/postgres.rs` (`fetch_pending`, `OUTBOX_FETCH_PENDING_SQL`)
- `crates/messaging/src/outbox/mysql.rs` (`fetch_pending`, `OUTBOX_FETCH_PENDING_SQL`)
- `crates/messaging/src/outbox/sql/postgres/outbox_fetch_pending.sql` (new)
- `crates/messaging/src/outbox/sql/mysql/outbox_fetch_pending.sql` (new)
- `crates/data/src/persistence/sql/postgres/jobs_list_dead_letters.sql`
- `crates/messaging/src/jobs/postgres.rs` (`list_dead_letters`)

## Deliverable

1. **`crates/messaging/src/outbox/sqlite.rs` (`claim_pending_with_lease`):**
   - Replace `self.pool().begin().await` with `self.pool().begin_with("BEGIN IMMEDIATE").await` (`sqlx::Pool::begin_with` verified in `sqlx-core 0.9.0`) so SQLite acquires the write lock up front instead of attempting a `SHARED -> RESERVED` lock upgrade after `SELECT` (which bypasses `busy_timeout` with `SQLITE_BUSY`).
   - Replace `.fetch_one(&mut *tx)` on `SQLITE_OUTBOX_CLAIM_LEASE_SQL` with `.fetch_optional(&mut *tx)` and skip `None` defensively rather than failing the batch with `RowNotFound`.
2. **`crates/messaging/src/outbox/{postgres,mysql}.rs`:**
   - Add `crates/messaging/src/outbox/sql/postgres/outbox_fetch_pending.sql` and `crates/messaging/src/outbox/sql/mysql/outbox_fetch_pending.sql` matching `outbox_claim.sql` without `FOR UPDATE SKIP LOCKED`.
   - Use `OUTBOX_FETCH_PENDING_SQL` in `OutboxRepository::fetch_pending` for Postgres and MySQL so read-only inspection neither acquires row locks nor skips rows locked by concurrent writers.
3. **`crates/data/src/persistence/sql/postgres/jobs_list_dead_letters.sql` & `crates/messaging/src/jobs/postgres.rs:475`:**
   - Append `, trace_context` to the `SELECT` column list in `jobs_list_dead_letters.sql`.
   - Replace `r.try_get("trace_context").ok()` at `postgres.rs:475` with `r.try_get::<Option<String>, _>("trace_context").map_err(|e| JobError::Fatal(e.to_string()))?` so schema/projection regressions fail loud.

## Constraints honoured

- `rules/70-code-rationale.md`:
  - `Why:` comment on `begin_with("BEGIN IMMEDIATE")` in `sqlite.rs` explaining why deferred `BEGIN` + `SELECT` + `UPDATE` causes immediate `SQLITE_BUSY` without waiting on `busy_timeout` (unlike `BEGIN IMMEDIATE`), and why per-row `Uuid` lease tokens prevent switching to a single `UPDATE ... RETURNING`.
  - `Why:` comment on `OUTBOX_FETCH_PENDING_SQL` explaining `fetch_pending` is a non-claiming read inspection path while `claim_pending_with_lease` owns `FOR UPDATE SKIP LOCKED`.
- `crates/core/tests/integration/outbox.rs` (`outbox_claim_lease_recovery_of_abandoned_messages`) and `crates/core/tests/containers/{postgres,mysql}.rs` (`SKIP LOCKED` mutual-exclusion tests).

## Named tests (bodies omitted)

- `sqlite_concurrent_outbox_claims_with_immediate_tx_never_hit_busy_or_duplicate` (`crates/messaging/src/outbox/sqlite.rs` `#[cfg(test)]`) — uses a multi-connection temp-file SQLite pool (`max_connections >= 2`), seeds pending messages, spawns concurrent `claim_pending_with_lease` callers, and asserts zero `SQLITE_BUSY` / `RowNotFound` errors and strictly disjoint claimed IDs.
- `postgres_and_mysql_outbox_fetch_pending_sql_omits_for_update_skip_locked` (`crates/messaging/src/outbox/postgres.rs` & `mysql.rs` `#[cfg(test)]`) — verifies `OUTBOX_FETCH_PENDING_SQL` omits `FOR UPDATE` / `SKIP LOCKED` while `OUTBOX_CLAIM_SQL` retains `FOR UPDATE SKIP LOCKED`, plus `test_postgres_fetch_pending_does_not_skip_locked_rows` in `crates/core/tests/containers/postgres.rs` verifying `fetch_pending` remains readable alongside an open claim transaction.
- `postgres_list_dead_letters_preserves_trace_context` (`crates/core/tests/containers/postgres.rs`) — enqueues a job with an explicit `traceparent`, dead-letters it, calls `list_dead_letters(10)`, and asserts `trace_context` equals `Some(traceparent)`.

## Verification

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -j 2 -- -D warnings
cargo test -p rume-messaging --all-features -j 2
cargo test -p rume --test integration --all-features outbox -j 2
```

(And with Docker running: `cargo test -p rume --test containers --all-features postgres -j 2`.)

Expected: 0 warnings and all outbox/jobs tests pass.

## Commit

Only when the user asks:

```text
fix(messaging): use SQLite BEGIN IMMEDIATE claiming, non-locking fetch_pending, and DLQ trace_context
```

## Done-when

- [ ] `crates/messaging/src/outbox/sqlite.rs` uses `begin_with("BEGIN IMMEDIATE")` and `fetch_optional`.
- [ ] `outbox_fetch_pending.sql` exists for `postgres` and `mysql` without `FOR UPDATE SKIP LOCKED`, and `fetch_pending` uses it.
- [ ] `jobs_list_dead_letters.sql` selects `trace_context` and `crates/messaging/src/jobs/postgres.rs:475` propagates `try_get` errors.
- [ ] All verification commands exit 0 with 0 warnings.
