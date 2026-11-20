# Task 09 — Move SQL tree to `crates/data/sql/`; create `rume_data::sql`

## Goal

Single shared SQL tree at `crates/data/sql/` (all dialects), depth ≤ 4;
`crates/data/src/persistence/sql{,.rs}` deleted; messaging SQL included via `rume_data::sql::…`
consts. `cargo build` green after the task.

## Files touched

**Tree moves (git mv; file contents verbatim):**
- `crates/data/src/persistence/sql/` → `crates/data/sql/` (50 files: `common/{audit_count,migration_list_applied,ping}.sql`,
  `mysql/{13}`, `postgres/{23}`, `sqlite/{11}` — exact paths from `find crates/data/src/persistence/sql -type f`).
- `crates/messaging/src/jobs/sql/postgres/*.sql` (14 files: jobs_ack, jobs_claim, jobs_dead_letter,
  jobs_dead_letter_count, jobs_defer, jobs_idempotency_check, jobs_insert, jobs_list_dead_letters,
  jobs_nack, jobs_pending_count, jobs_purge, jobs_reap, jobs_renew_lease, jobs_replay_dead_letter)
  → `crates/data/sql/postgres/` — no name collisions with the data tree (verified: all 59 messaging
  SQL files are unique vs the data tree; `diff -q` over same-named data files shows no identical or
  differing pairs).
- `crates/messaging/src/outbox/sql/mysql/*.sql` (16) → `crates/data/sql/mysql/`;
  `crates/messaging/src/outbox/sql/postgres/*.sql` (15) → `crates/data/sql/postgres/`;
  `crates/messaging/src/outbox/sql/sqlite/*.sql` (14) → `crates/data/sql/sqlite/` — likewise
  name-unique vs the data tree.
- Deleted: `crates/messaging/src/jobs/sql/`, `crates/messaging/src/outbox/sql/`.

**Delete + module:**
- Delete `crates/data/src/persistence/sql.rs`; delete `crates/data/src/persistence/sql/`.
- Delete `crates/data/src/persistence/mod.rs` line 17 `pub mod sql;` (crates/data/src/persistence/mod.rs).
- Add `crates/data/src/sql.rs`; add `crates/data/src/lib.rs` line `pub mod sql;`
  (next to `pub mod cache; pub mod coordination; pub mod persistence;`).

**`crates/data/src/sql.rs` (new module `rume_data::sql`)** — consts, `include_str!("../sql/…")`
(file at `crates/data/src/sql.rs` → tree at `crates/data/sql/`):
- Moved from the deleted `crates/data/src/persistence/sql.rs`:
  - `pub const PG_JOBS_SCHEMA` (→ `../sql/postgres/jobs_schema.sql`)
  - `pub const PG_AUDIT_SCHEMA` (→ `../sql/postgres/audit_schema.sql`)
  - `pub const MYSQL_AUDIT_SCHEMA` (→ `../sql/mysql/audit_schema.sql`)
  - `pub const SQLITE_AUDIT_SCHEMA` (→ `../sql/sqlite/audit_schema.sql`)
  - `pub const AUDIT_COUNT_SQL` (→ `../sql/common/audit_count.sql`)
  - `pub const MYSQL_SET_ISOLATION_LEVEL` (→ `../sql/mysql/set_isolation_level.sql`)
- New consts moved from the messaging trees (identical SQL text, moved verbatim — the moved
  `.sql` files are the same bytes):
  - Jobs (14; `crates/messaging/src/jobs/postgres.rs` const → `rume_data::sql::`):
    `PG_JOBS_CLAIM` (→ `../sql/postgres/jobs_claim.sql`), `PG_JOBS_INSERT` (jobs_insert.sql),
    `PG_JOBS_REAP` (jobs_reap.sql), `PG_JOBS_PENDING_COUNT` (jobs_pending_count.sql),
    `PG_JOBS_DEAD_LETTER_COUNT` (jobs_dead_letter_count.sql), `PG_JOBS_IDEMPOTENCY_CHECK`
    (jobs_idempotency_check.sql), `PG_JOBS_ACK` (jobs_ack.sql), `PG_JOBS_NACK` (jobs_nack.sql),
    `PG_JOBS_DEFER` (jobs_defer.sql), `PG_JOBS_DEAD_LETTER` (jobs_dead_letter.sql),
    `PG_JOBS_RENEW_LEASE` (jobs_renew_lease.sql), `PG_JOBS_LIST_DEAD_LETTERS` (jobs_list_dead_letters.sql),
    `PG_JOBS_REPLAY_DEAD_LETTER` (jobs_replay_dead_letter.sql), `PG_JOBS_PURGE` (jobs_purge.sql).
  - Outbox MySQL (16; `crates/messaging/src/outbox/mysql.rs`):
    `MYSQL_OUTBOX_CLAIM` (outbox_claim.sql), `MYSQL_OUTBOX_FETCH_PENDING` (outbox_fetch_pending.sql),
    `MYSQL_OUTBOX_INSERT` (outbox_insert.sql), `MYSQL_OUTBOX_RECOVER` (outbox_recover.sql),
    `MYSQL_OUTBOX_CLAIM_LEASE` (outbox_claim_lease.sql), `MYSQL_OUTBOX_SELECT_LEASE_EXPIRES_AT`
    (outbox_select_lease_expires_at.sql), `MYSQL_OUTBOX_RENEW_LEASE` (outbox_renew_lease.sql),
    `MYSQL_OUTBOX_MARK_PUBLISHED` (outbox_mark_published.sql), `MYSQL_OUTBOX_SELECT_RETRY_COUNT`
    (outbox_select_retry_count.sql), `MYSQL_OUTBOX_MARK_FAILED` (outbox_mark_failed.sql),
    `MYSQL_OUTBOX_PENDING_COUNT` (outbox_pending_count.sql), `MYSQL_OUTBOX_PUBLISHED_COUNT`
    (outbox_published_count.sql), `MYSQL_OUTBOX_FAILED_COUNT` (outbox_failed_count.sql),
    `MYSQL_OUTBOX_FETCH_FAILED` (outbox_fetch_failed.sql), `MYSQL_OUTBOX_REPROCESS_FAILED`
    (outbox_reprocess_failed.sql), `MYSQL_OUTBOX_PURGE_PUBLISHED` (outbox_purge_published.sql).
  - Outbox PostgreSQL (15; `crates/messaging/src/outbox/postgres.rs`):
    `PG_OUTBOX_CLAIM` (outbox_claim.sql), `PG_OUTBOX_FETCH_PENDING` (outbox_fetch_pending.sql),
    `PG_OUTBOX_INSERT` (outbox_insert.sql), `PG_OUTBOX_RECOVER` (outbox_recover.sql),
    `PG_OUTBOX_CLAIM_LEASE` (outbox_claim_lease.sql), `PG_OUTBOX_RENEW_LEASE` (outbox_renew_lease.sql),
    `PG_OUTBOX_MARK_PUBLISHED` (outbox_mark_published.sql), `PG_OUTBOX_SELECT_RETRY_COUNT`
    (outbox_select_retry_count.sql), `PG_OUTBOX_MARK_FAILED` (outbox_mark_failed.sql),
    `PG_OUTBOX_PENDING_COUNT` (outbox_pending_count.sql), `PG_OUTBOX_PUBLISHED_COUNT`
    (outbox_published_count.sql), `PG_OUTBOX_FAILED_COUNT` (outbox_failed_count.sql),
    `PG_OUTBOX_FETCH_FAILED` (outbox_fetch_failed.sql), `PG_OUTBOX_REPROCESS_FAILED`
    (outbox_reprocess_failed.sql), `PG_OUTBOX_PURGE_PUBLISHED` (outbox_purge_published.sql).
  - Outbox SQLite (14; `crates/messaging/src/outbox/sqlite.rs`):
    `SQLITE_OUTBOX_CLAIM` (outbox_claim.sql), `SQLITE_OUTBOX_INSERT` (outbox_insert.sql),
    `SQLITE_OUTBOX_RECOVER` (outbox_recover.sql), `SQLITE_OUTBOX_CLAIM_LEASE` (outbox_claim_lease.sql),
    `SQLITE_OUTBOX_RENEW_LEASE` (outbox_renew_lease.sql), `SQLITE_OUTBOX_MARK_PUBLISHED`
    (outbox_mark_published.sql), `SQLITE_OUTBOX_SELECT_RETRY_COUNT` (outbox_select_retry_count.sql),
    `SQLITE_OUTBOX_MARK_FAILED` (outbox_mark_failed.sql), `SQLITE_OUTBOX_PENDING_COUNT`
    (outbox_pending_count.sql), `SQLITE_OUTBOX_PUBLISHED_COUNT` (outbox_published_count.sql),
    `SQLITE_OUTBOX_FAILED_COUNT` (outbox_failed_count.sql), `SQLITE_OUTBOX_FETCH_FAILED`
    (outbox_fetch_failed.sql), `SQLITE_OUTBOX_REPROCESS_FAILED` (outbox_reprocess_failed.sql),
    `SQLITE_OUTBOX_PURGE_PUBLISHED` (outbox_purge_published.sql).

**Data-internal include path fixes (tree moved):**
- `crates/data/src/persistence/backend/mod.rs` (Task 06): `sql/postgres/audit_insert.sql`,
  `sql/mysql/audit_insert.sql`, `sql/sqlite/audit_insert.sql` → `../../../sql/postgres/audit_insert.sql`
  etc. (file at `crates/data/src/persistence/backend/mod.rs`).
- `crates/data/src/persistence/migration/preflight.rs` (Task 05): all `include_str!("sql/…")`
  → `../../../sql/…`.
- `crates/data/src/persistence/postgres.rs`: `include_str!("sql/postgres/…")`, `include_str!("sql/common/ping.sql")`
  → `../../sql/…`.
- `crates/data/src/persistence/mysql.rs`: same pattern.
- `crates/data/src/persistence/sqlite.rs`: same pattern.
- `crates/data/src/coordination/postgres.rs`: `include_str!("../persistence/sql/postgres/locks_schema.sql")` etc.
  → `../../sql/postgres/locks_schema.sql` etc.

**Messaging consumers (consts replace `include_str!`):**
- `crates/messaging/src/jobs/postgres.rs`: `JOBS_SCHEMA` → `rume_data::sql::PG_JOBS_SCHEMA`;
  `JOBS_CLAIM_SQL` → `rume_data::sql::PG_JOBS_CLAIM`; `INSERT_JOB_SQL` → `rume_data::sql::PG_JOBS_INSERT`;
  `JOBS_REAP_SQL` → `rume_data::sql::PG_JOBS_REAP`; `JOBS_PENDING_COUNT_SQL` → `rume_data::sql::PG_JOBS_PENDING_COUNT`;
  `JOBS_DEAD_LETTER_COUNT_SQL` → `rume_data::sql::PG_JOBS_DEAD_LETTER_COUNT`;
  `JOBS_IDEMPOTENCY_CHECK_SQL` → `rume_data::sql::PG_JOBS_IDEMPOTENCY_CHECK`; `JOBS_ACK_SQL` →
  `rume_data::sql::PG_JOBS_ACK`; `JOBS_NACK_SQL` → `rume_data::sql::PG_JOBS_NACK`; `JOBS_DEFER_SQL` →
  `rume_data::sql::PG_JOBS_DEFER`; `JOBS_DEAD_LETTER_SQL` → `rume_data::sql::PG_JOBS_DEAD_LETTER`;
  `JOBS_RENEW_LEASE_SQL` → `rume_data::sql::PG_JOBS_RENEW_LEASE`; `JOBS_LIST_DEAD_LETTERS_SQL` →
  `rume_data::sql::PG_JOBS_LIST_DEAD_LETTERS`; `JOBS_REPLAY_DEAD_LETTER_SQL` →
  `rume_data::sql::PG_JOBS_REPLAY_DEAD_LETTER`; `JOBS_PURGE_SQL` → `rume_data::sql::PG_JOBS_PURGE`.
- `crates/messaging/src/outbox/mysql.rs`: `MYSQL_SET_ISOLATION_LEVEL_SQL` → `rume_data::sql::MYSQL_SET_ISOLATION_LEVEL`;
  `OUTBOX_CLAIM_SQL` → `rume_data::sql::MYSQL_OUTBOX_CLAIM`; `OUTBOX_FETCH_PENDING_SQL` →
  `rume_data::sql::MYSQL_OUTBOX_FETCH_PENDING`; `INSERT_OUTBOX_SQL` → `rume_data::sql::MYSQL_OUTBOX_INSERT`;
  `OUTBOX_RECOVER_SQL` → `rume_data::sql::MYSQL_OUTBOX_RECOVER`; `MYSQL_OUTBOX_CLAIM_LEASE_SQL` →
  `rume_data::sql::MYSQL_OUTBOX_CLAIM_LEASE`; `MYSQL_OUTBOX_SELECT_LEASE_EXPIRES_AT_SQL` →
  `rume_data::sql::MYSQL_OUTBOX_SELECT_LEASE_EXPIRES_AT`; `MYSQL_OUTBOX_RENEW_LEASE_SQL` →
  `rume_data::sql::MYSQL_OUTBOX_RENEW_LEASE`; `MYSQL_OUTBOX_MARK_PUBLISHED_SQL` →
  `rume_data::sql::MYSQL_OUTBOX_MARK_PUBLISHED`; `MYSQL_OUTBOX_SELECT_RETRY_COUNT_SQL` →
  `rume_data::sql::MYSQL_OUTBOX_SELECT_RETRY_COUNT`; `MYSQL_OUTBOX_MARK_FAILED_SQL` →
  `rume_data::sql::MYSQL_OUTBOX_MARK_FAILED`; `MYSQL_OUTBOX_PENDING_COUNT_SQL` →
  `rume_data::sql::MYSQL_OUTBOX_PENDING_COUNT`; `MYSQL_OUTBOX_PUBLISHED_COUNT_SQL` →
  `rume_data::sql::MYSQL_OUTBOX_PUBLISHED_COUNT`; `MYSQL_OUTBOX_FAILED_COUNT_SQL` →
  `rume_data::sql::MYSQL_OUTBOX_FAILED_COUNT`; `MYSQL_OUTBOX_FETCH_FAILED_SQL` →
  `rume_data::sql::MYSQL_OUTBOX_FETCH_FAILED`; `MYSQL_OUTBOX_REPROCESS_FAILED_SQL` →
  `rume_data::sql::MYSQL_OUTBOX_REPROCESS_FAILED`; `MYSQL_OUTBOX_PURGE_PUBLISHED_SQL` →
  `rume_data::sql::MYSQL_OUTBOX_PURGE_PUBLISHED`.
- `crates/messaging/src/outbox/postgres.rs`: `OUTBOX_CLAIM_SQL` → `rume_data::sql::PG_OUTBOX_CLAIM`;
  `OUTBOX_FETCH_PENDING_SQL` → `rume_data::sql::PG_OUTBOX_FETCH_PENDING`; `INSERT_OUTBOX_SQL` →
  `rume_data::sql::PG_OUTBOX_INSERT`; `OUTBOX_RECOVER_SQL` → `rume_data::sql::PG_OUTBOX_RECOVER`;
  `PG_OUTBOX_CLAIM_LEASE_SQL` → `rume_data::sql::PG_OUTBOX_CLAIM_LEASE`; `PG_OUTBOX_RENEW_LEASE_SQL` →
  `rume_data::sql::PG_OUTBOX_RENEW_LEASE`; `PG_OUTBOX_MARK_PUBLISHED_SQL` →
  `rume_data::sql::PG_OUTBOX_MARK_PUBLISHED`; `PG_OUTBOX_SELECT_RETRY_COUNT_SQL` →
  `rume_data::sql::PG_OUTBOX_SELECT_RETRY_COUNT`; `PG_OUTBOX_MARK_FAILED_SQL` →
  `rume_data::sql::PG_OUTBOX_MARK_FAILED`; `PG_OUTBOX_PENDING_COUNT_SQL` →
  `rume_data::sql::PG_OUTBOX_PENDING_COUNT`; `PG_OUTBOX_PUBLISHED_COUNT_SQL` →
  `rume_data::sql::PG_OUTBOX_PUBLISHED_COUNT`; `PG_OUTBOX_FAILED_COUNT_SQL` →
  `rume_data::sql::PG_OUTBOX_FAILED_COUNT`; `PG_OUTBOX_FETCH_FAILED_SQL` →
  `rume_data::sql::PG_OUTBOX_FETCH_FAILED`; `PG_OUTBOX_REPROCESS_FAILED_SQL` →
  `rume_data::sql::PG_OUTBOX_REPROCESS_FAILED`; `PG_OUTBOX_PURGE_PUBLISHED_SQL` →
  `rume_data::sql::PG_OUTBOX_PURGE_PUBLISHED`.
- `crates/messaging/src/outbox/sqlite.rs`: `OUTBOX_CLAIM_SQL` → `rume_data::sql::SQLITE_OUTBOX_CLAIM`;
  `INSERT_OUTBOX_SQL` → `rume_data::sql::SQLITE_OUTBOX_INSERT`; `OUTBOX_RECOVER_SQL` →
  `rume_data::sql::SQLITE_OUTBOX_RECOVER`; `SQLITE_OUTBOX_CLAIM_LEASE_SQL` →
  `rume_data::sql::SQLITE_OUTBOX_CLAIM_LEASE`; `SQLITE_OUTBOX_RENEW_LEASE_SQL` →
  `rume_data::sql::SQLITE_OUTBOX_RENEW_LEASE`; `SQLITE_OUTBOX_MARK_PUBLISHED_SQL` →
  `rume_data::sql::SQLITE_OUTBOX_MARK_PUBLISHED`; `SQLITE_OUTBOX_SELECT_RETRY_COUNT_SQL` →
  `rume_data::sql::SQLITE_OUTBOX_SELECT_RETRY_COUNT`; `SQLITE_OUTBOX_MARK_FAILED_SQL` →
  `rume_data::sql::SQLITE_OUTBOX_MARK_FAILED`; `SQLITE_OUTBOX_PENDING_COUNT_SQL` →
  `rume_data::sql::SQLITE_OUTBOX_PENDING_COUNT`; `SQLITE_OUTBOX_PUBLISHED_COUNT_SQL` →
  `rume_data::sql::SQLITE_OUTBOX_PUBLISHED_COUNT`; `SQLITE_OUTBOX_FAILED_COUNT_SQL` →
  `rume_data::sql::SQLITE_OUTBOX_FAILED_COUNT`; `SQLITE_OUTBOX_FETCH_FAILED_SQL` →
  `rume_data::sql::SQLITE_OUTBOX_FETCH_FAILED`; `SQLITE_OUTBOX_REPROCESS_FAILED_SQL` →
  `rume_data::sql::SQLITE_OUTBOX_REPROCESS_FAILED`; `SQLITE_OUTBOX_PURGE_PUBLISHED_SQL` →
  `rume_data::sql::SQLITE_OUTBOX_PURGE_PUBLISHED`.

**Other consumers of `rume_data::persistence::sql::*`:**
- `crates/security/src/security/audit.rs:125-132`: `rume_data::persistence::sql::{PG_AUDIT_SCHEMA,
  MYSQL_AUDIT_SCHEMA, SQLITE_AUDIT_SCHEMA, AUDIT_COUNT_SQL}` → `rume_data::sql::…`.
- `crates/core/src/lib.rs`: add `pub use rume_data::sql;` (umbrella path `rume::sql`).
- `crates/core/tests/integration/persistence.rs:624`: `rume::persistence::sql::SQLITE_AUDIT_SCHEMA` →
  `rume::sql::SQLITE_AUDIT_SCHEMA`.

## Deliverable

- One SQL tree: `crates/data/sql/{common,mysql,postgres,sqlite}/*.sql` — 109 files
  (50 data + 59 messaging, all moved verbatim; `workbench/` untouched).
- `rume_data::sql` module (`crates/data/src/sql.rs`) — 64 consts above; `rume_data::sql::PG_JOBS_SCHEMA`
  is the single shared home for the jobs schema.
- Messaging SQL is consumed via `rume_data::sql::…` consts; no `include_str!("sql/…)` in
  `crates/messaging/src/jobs/` or `crates/messaging/src/outbox/` remains for SQL.
- `cargo build` green after the task.

## Constraints honoured

- Global constraints: pure refactor; no behavior changes (SQL text identical — moved verbatim);
  no Cargo.toml changes; `cargo build` green.
- Conductor decision 2: centralize all SQL in `crates/data/sql/`; delete
  `crates/data/src/persistence/sql{,.rs}`; messaging updated to `rume_data::sql::…`.
- `PG_JOBS_SCHEMA` was already shared (single copy in data; consumed via
  `rume_data::persistence::sql::PG_JOBS_SCHEMA`) — now at `rume_data::sql::PG_JOBS_SCHEMA`.
- Conductor Q4: workbench trees left as-is.
- Phase 2 ends with `cargo build` + `cargo test -p rume-data -p rume-messaging -p rume-core`
  green (tasks 09-10 gate).

## Named tests

No new tests. Suites that must stay green (existing):
- `rume-data`: `crates/data/src/persistence/backend.rs` validation proptests + unit tests
  (`validate_managed_transaction_sql_*`, `rejects_transaction_control_statements_in_raw_batches`, …),
  `crates/data/src/persistence/migration/tests.rs` (incl. `core_down_scripts_never_drop_ledger_or_audit_tables`,
  `with_rume_core_registers_core_and_audit_ahead_of_app_migrations`,
  `select_rollback_target_prefers_actual_application_order_over_registry_order`),
  `crates/data/src/persistence/backend/mod.rs` tests (`db_value_from_usize_handles_values_above_i64_max`,
  `map_sqlx_error_preserves_driver_classifications`), plus data unit tests in persistence backends,
  `crates/data/src/coordination/` tests, `crates/data/src/cache/` tests.
- `rume-messaging`: `crates/messaging/src/outbox/{postgres,mysql,sqlite}.rs` `mod tests` (moved
  verbatim, with the SQL consts updated to `rume_data::sql::…`), `crates/messaging/src/outbox/repository.rs`
  tests, `crates/messaging/src/outbox/relay.rs`, `crates/messaging/src/events/bus.rs` tests, and
  `crates/messaging/src/jobs/postgres.rs` (SQL consts now `rume_data::sql::…`; the module itself is a
  note, not a task — README observations).
- `rume-core`: `crates/core/tests/integration/persistence.rs` (uses `rume::sql::SQLITE_AUDIT_SCHEMA`
  at line 624), `crates/core/tests/integration/sqlite.rs` (`rume::persistence::migration::RUME_CORE_MIGRATION_ID`),
  `crates/core/tests/integration/enterprise.rs` (`rume::enterprise::*`), container suites.

## Verification

```
cargo build
cargo test -p rume-data -p rume-messaging -p rume-core
find crates/data/sql -type d | awk -F/ '{print NF}' | sort -n | tail -1   # expect: 4
find crates/messaging -name '*.sql'                                        # expect: empty
grep -rn "persistence::sql::" crates --include=*.rs                         # expect: empty
```

## Commit

Only when the user asks:

```
refactor(data): move SQL tree to crates/data/sql/ and add rume_data::sql module
```

## Done-when

- [ ] `crates/data/sql/` holds the single tree (109 `.sql` files: `find crates/data/sql -name '*.sql' | wc -l` = 109).
- [ ] `crates/data/src/persistence/sql.rs` and `crates/data/src/persistence/sql/` gone.
- [ ] `crates/data/src/sql.rs` defines `rume_data::sql` with the 64 consts above;
  `rume_data::sql::PG_JOBS_SCHEMA` resolves.
- [ ] `crates/messaging/src/jobs/sql/` and `crates/messaging/src/outbox/sql/` gone;
  `find crates/messaging -name '*.sql'` empty.
- [ ] `cargo build` green.
