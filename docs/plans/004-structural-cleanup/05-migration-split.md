# Task 05 — Split `crates/data/src/persistence/migration.rs`

## Goal

Split `crates/data/src/persistence/migration.rs` (1,817 lines) into
`crates/data/src/persistence/migration/{mod,preflight,runner,tests}.rs` — behavior-preserving;
`cargo build` + `cargo test -p rume-data` green.

## Files touched

- `crates/data/src/persistence/migration.rs` — deleted; replaced by:
- `crates/data/src/persistence/migration/mod.rs` — `mod preflight; mod runner;` + re-exports
  `pub use runner::{Migration, MigrationSummary, Migrator, RUME_AUDIT_MIGRATION_ID, RUME_CORE_MIGRATION_ID};`
  (so `rume::persistence::migration::RUME_CORE_MIGRATION_ID` still resolves — used by
  `crates/core/tests/integration/persistence.rs:516,553,558,573,666,715` and
  `crates/core/tests/integration/sqlite.rs:154`); `#[cfg(test)] mod tests;` → `migration/tests.rs`.
- `crates/data/src/persistence/migration/preflight.rs` — the SQL consts + preflight/apply machinery:
  `RUME_CORE_MIGRATION_ID`? — no: `RUME_CORE_MIGRATION_ID`/`RUME_AUDIT_MIGRATION_ID` live in
  `runner.rs` (see below); consts: `PG_OUTBOX` (12), `PG_JOBS` (13), `PG_LOCKS` (14), `PG_LEADER` (15),
  `PG_AUDIT` (16), `PG_CORE_DOWN` (17), `MYSQL_OUTBOX` (19), `MYSQL_AUDIT` (20), `MYSQL_CORE_DOWN` (21),
  `SQLITE_OUTBOX` (23), `SQLITE_AUDIT` (24), `SQLITE_CORE_DOWN` (25), `PG_MIGRATIONS_SCHEMA` (27),
  `MYSQL_MIGRATIONS_SCHEMA` (28), `SQLITE_MIGRATIONS_SCHEMA` (29), `CREATE_MIGRATIONS_TABLE_DDL` (32),
  `migrations_schema_ddl` (35), `PG_MIGRATION_FIND_CHECKSUM` (45), `PG_MIGRATION_INSERT` (46),
  `PG_MIGRATION_DELETE` (47), `PG_MIGRATION_FIND_ID` (48), `PG_MIGRATION_LIST_APPLIED` (49),
  `PG_ADVISORY_LOCK` (50), `PG_CLOCK` (51), `MYSQL_MIGRATION_FIND_CHECKSUM` (53),
  `MYSQL_MIGRATION_INSERT` (54), `MYSQL_MIGRATION_DELETE` (55), `MYSQL_MIGRATION_FIND_ID` (56),
  `MYSQL_CLOCK` (58), `MYSQL_MIGRATION_LOCK` (60), `MYSQL_MIGRATION_UNLOCK` (62),
  `SQLITE_MIGRATION_FIND_CHECKSUM` (64), `SQLITE_MIGRATION_INSERT` (65), `SQLITE_MIGRATION_DELETE` (66),
  `SQLITE_MIGRATION_FIND_ID` (67), `SQLITE_CLOCK` (68), `SQLITE_BUSY_TIMEOUT` (69),
  `LIST_APPLIED_MIGRATIONS_SQL` (103), `NEXT_APPLIED_ORDER_SQL` (104), `APPLIED_ORDER_MYSQL_COUNT_SQL` (107,
  `#[cfg(feature = "mysql")]`), `APPLIED_ORDER_SQLITE_COUNT_SQL` (109), `LIST_APPLIED_ORDER_SQL` (110),
  `find_checksum_query` (71), `insert_migration_query` (79), `delete_migration_query` (87),
  `find_migration_id_query` (95), `select_rollback_target` (112), `ensure_applied_order_column_tx` (134),
  `ensure_applied_order_column_backend` (170), `ensure_applied_order_column_mysql` (275,
  `#[cfg(feature = "mysql")]`), `next_applied_order_tx` (252), `next_applied_order_mysql` (299,
  `#[cfg(feature = "mysql")]`), `fetch_applied_order_rows_tx` (260), `fetch_applied_order_rows_mysql` (310,
  `#[cfg(feature = "mysql")]`), `parse_clock_or_now` (344).
  Visibility: `pub(super)` consts/fns used by `runner.rs`.
- `crates/data/src/persistence/migration/runner.rs` — `RUME_CORE_MIGRATION_ID` (7), `RUME_AUDIT_MIGRATION_ID` (10),
  `Migration` (575-637), `MigrationSummary` (640-644), `Migrator` (648-1816) + impl: `new` (654),
  `with_rume_core` (661-703), `add_migration` (706), `migrations` (712), `pending_migrations` (717),
  `applied_migrations` (746), `run_pending` (764), `rollback_last` (775), `verify_no_pending` (783),
  `verify_applied_checksums` (793), `run_pending_backend` (819-1095), `rollback_last_backend`
  (1098-1816) + MySQL session helpers: `mysql_rollback_then_unlock` (448), `MysqlLockState` (457),
  `MysqlMigrationLockGuard` (477-572), `bind_mysql_conn_params` (359), `extract_mysql_scalar` (377),
  `mysql_conn_execute_raw` (398), `mysql_conn_execute_params` (410), `mysql_conn_query_scalar_params` (424).
- `crates/data/src/persistence/migration/tests.rs` — the `#[cfg(test)] mod tests` moved verbatim
  (migration.rs:1311+).

## Deliverable

`rume_data::persistence::Migrator`/`Migration`/`MigrationSummary` + `rume::persistence::migration::RUME_CORE_MIGRATION_ID`
resolve as before; preflight/apply machinery split by concern (preflight queries, rollback-target
selection, applied-order schema handling vs. the run/rollback loops + MySQL session locking).
No behavior changes.

## Constraints honoured

- `crates/data/src/persistence/mod.rs:9` `pub use migration::{Migration, MigrationSummary, Migrator};`
  unchanged — via `migration/mod.rs` re-exports.
- `crates/core/tests/integration/persistence.rs:516,553,558,573,666,715` and
  `crates/core/tests/integration/sqlite.rs:154` use `rume::persistence::migration::RUME_CORE_MIGRATION_ID`
  — via `migration/mod.rs` re-export.
- `cargo build` + `cargo test -p rume-data` green.

## Named tests (bodies omitted — moved from `crates/data/src/persistence/migration.rs:1311+`)

From `migration/tests.rs`:
- `core_down_scripts_never_drop_ledger_or_audit_tables` — asserts the `core_down.sql` scripts never drop `rume_migrations` or `rume_audit_log`.
- `with_rume_core_registers_core_and_audit_ahead_of_app_migrations` — asserts `with_rume_core` registers core/audit migrations ahead of app migrations.
- `mysql_dialect_without_migration_lock_capability_fails_closed_before_mutation` — asserts MySQL migrations fail closed when `mysql_migration_pool()` is missing.
- `select_rollback_target_prefers_actual_application_order_over_registry_order` — asserts rollback target selection uses persisted `applied_order`.
- `generic_backend_fallback_ensures_applied_order_column_before_insert` — asserts the `applied_order` column is ensured before the ledger insert on custom backends.

## Verification

```
cargo build
cargo test -p rume-data
```

## Commit

Only when the user asks:

```
refactor(data): split persistence/migration.rs into migration/ preflight and runner modules
```

## Done-when

- [ ] `crates/data/src/persistence/migration/` contains `mod.rs`, `preflight.rs`, `runner.rs`, `tests.rs`;
  `crates/data/src/persistence/migration.rs` deleted.
- [ ] `rume_data::persistence::{Migrator, Migration, MigrationSummary}` and
  `rume::persistence::migration::RUME_CORE_MIGRATION_ID` resolve.
- [ ] `cargo build` + `cargo test -p rume-data` green.
