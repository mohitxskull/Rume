# Task 02 — Core Migration Ledger Preservation & MySQL Per-Migration Atomicity (`RC-02`)

## Goal

Prevent core rollbacks from dropping `rume_migrations` and `rume_audit_log`, register `rume_audit_log` as a second append-only core migration (`20261104_002_rume_audit_schema`) without altering `20261104_001_rume_core_schemas`'s checksum, and remove fake multi-migration `START TRANSACTION` / `COMMIT` / `ROLLBACK` wrappers from MySQL `run_pending_backend` / `rollback_last_backend` by executing directly on a `GET_LOCK`-pinned `PoolConnection<MySql>`.

## Files touched

- `crates/data/src/persistence/sql/postgres/core_down.sql`
- `crates/data/src/persistence/sql/mysql/core_down.sql`
- `crates/data/src/persistence/sql/sqlite/core_down.sql`
- `crates/data/src/persistence/migration.rs` (`RUME_AUDIT_MIGRATION_ID`, `Migrator::with_rume_core`, `run_pending_backend`, `rollback_last_backend`, `MysqlMigrationLockGuard`)
- `crates/core/tests/integration/persistence.rs` (`migrator_with_rume_core_registers_and_applies_core_persistence` at lines 513–518)
- `crates/core/tests/integration/sqlite.rs` (`sqlite_auto_migrate_applies_pending_migrations`)
- `workbench/multi-instance/tests/deployment_validation.rs` (line 88 assertion)
- `workbench/multi-instance/tests/workbench_test.rs` (line 71 assertion)

## Deliverable

1. Remove `DROP TABLE IF EXISTS rume_audit_log;` and `DROP TABLE IF EXISTS rume_migrations;` from `core_down.sql` under `postgres/`, `mysql/`, and `sqlite/`.
2. Keep `RUME_CORE_MIGRATION_ID` (`20261104_001_rume_core_schemas`) `up_sql` byte-for-byte unchanged so existing SHA-256 checksums match.
3. Define `pub const RUME_AUDIT_MIGRATION_ID: &str = "20261104_002_rume_audit_schema";` in `crates/data/src/persistence/migration.rs` and insert it at index `1` inside `Migrator::with_rume_core(dialect)` (right after `RUME_CORE_MIGRATION_ID` at index `0`) with dialect-appropriate `audit_schema.sql` as `up_sql` and `"DROP TABLE IF EXISTS rume_audit_log;"` as `down_sql`.
4. Remove `backend.begin_transaction()`, `"START TRANSACTION"`, `"COMMIT"`, and `"ROLLBACK"` from MySQL `run_pending_backend` and `rollback_last_backend` (`crates/data/src/persistence/migration.rs:381-480,627-693`) — MySQL DDL implicitly auto-commits every statement. Under `#[cfg(feature = "mysql")]`, downcast `(&*backend as &dyn std::any::Any).downcast_ref::<crate::persistence::MysqlBackend>()` (`DatabaseBackend: 'static`) to acquire a single pinned connection `let mut conn = mysql_backend.pool().acquire().await.map_err(map_sqlx_error)?;` guarded by a private RAII `MysqlMigrationLockGuard` (spawning `MYSQL_MIGRATION_UNLOCK` on `Drop` if dropped early while still locked, matching `MysqlTransaction::drop`), acquire `MYSQL_MIGRATION_LOCK` (`"1"`) directly on `&mut *conn`, execute `ddl` directly, execute each pending migration's `up_sql` (`down_sql`) directly via `sqlx::raw_sql(AssertSqlSafe(...))` on `&mut *conn` followed immediately by its `rume_migrations` `INSERT` (`DELETE`) ledger query on `&mut *conn`, and explicitly execute `MYSQL_MIGRATION_UNLOCK` (`"1"`) before disarming the guard and returning the connection to the pool (falling back to non-transactional `backend` calls only for non-`MysqlBackend` mocks).
5. Update existing assertions for the second core migration: `persistence.rs:513-518` (`applied.len() == 3`, `applied[0] == RUME_CORE_MIGRATION_ID`, `applied[1] == RUME_AUDIT_MIGRATION_ID`, `applied[2] == "20261104_002_custom_app"`), and `.applied.len()` (`+1`) in `sqlite.rs:150`, `deployment_validation.rs:88`, and `workbench_test.rs:71`.

## Constraints honoured

- `rules/70-code-rationale.md` & `OQ-01`:
  - `Why:` comment in `with_rume_core` documenting why `20261104_001_rume_core_schemas` `up_sql` stays immutable (preserving deployed checksums) while `rume_audit_log` lives in append-only `20261104_002_rume_audit_schema` and `rume_migrations` is never dropped in `core_down.sql`.
  - `Why:` comment on MySQL `run_pending_backend` / `rollback_last_backend` and `MysqlMigrationLockGuard` documenting why MySQL's implicit DDL commit semantics make multi-migration `"START TRANSACTION"`/`"COMMIT"`/`"ROLLBACK"` misleading and non-atomic, and why pinning a single `PoolConnection<MySql>` directly via `MysqlBackend::pool().acquire()` preserves session-bound `GET_LOCK`/`RELEASE_LOCK` (`CONNECTION_ID()`) without starting a fake `begin_transaction()` transaction or adding `MysqlTransaction::execute_unvalidated` (revisit only if targeting a transactional-DDL MySQL fork).
- `crates/core/tests/containers/mysql.rs:407-511` (`test_mysql_migration_lock_ordering_and_durability`: session-bound `GET_LOCK`/`RELEASE_LOCK` invariant).
- `workbench/multi-instance/migrations/` & `workbench/simple-api/src/lib.rs` (calling `.with_rume_core(...)` after `embed_migrations!` still places `001` and `002` at indexes `0` and `1`).

## Named tests (bodies omitted)

- `migrator_with_rume_core_checksum_unchanged_and_rollback_preserves_ledger` (`crates/core/tests/integration/persistence.rs`) — verifies `RUME_CORE_MIGRATION_ID` checksum equals the exact pre-change digest of `outbox_schema.sql` (`sqlite` / `mysql` / concatenated `postgres`), applies `with_rume_core("sqlite")`, rolls back `20261104_002_rume_audit_schema` then `20261104_001_rume_core_schemas`, and asserts `rume_migrations` remains queryable with 0 rows after full rollback.
- `mysql_per_migration_records_prior_migration_when_later_migration_fails` (`crates/core/tests/containers/mysql.rs`) — runs two migrations where #1 creates a table and #2 fails mid-way, asserting #1 remains recorded in `rume_migrations` (no false outer rollback drift), #2 is absent, and `rume_migrations_lock` is released.

## Code-only decisions

- `core_down.sql`: keep only schema drops owned by `20261104_001_rume_core_schemas` (`rume_leaders`, `rume_locks`, `rume_jobs`, `rume_outbox`, and Postgres sequences `rume_leader_term_seq`, `rume_lock_fencing_seq`).

## Verification

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -j 2 -- -D warnings
cargo test -p rume-data --all-features -j 2
cargo test -p rume --test integration --all-features persistence -j 2
cargo test -p rume --test integration --all-features sqlite -j 2
cargo check -p workbench -p workbench-simple-api -p workbench-modular-monolith -j 2
```

(And with Docker running: `cargo test -p rume --test containers --all-features test_mysql_migration -j 2`.)

Expected: 0 warnings and all migration tests pass.

## Commit

Only when the user asks:

```text
fix(persistence): preserve migration ledger across core rollbacks and run MySQL migrations on a pinned connection
```

## Done-when

- [ ] `rg -n 'rume_migrations|rume_audit_log' crates/data/src/persistence/sql/*/core_down.sql` returns no matches.
- [ ] `RUME_AUDIT_MIGRATION_ID` (`20261104_002_rume_audit_schema`) is registered in `Migrator::with_rume_core` without modifying `20261104_001_rume_core_schemas` `up_sql`.
- [ ] MySQL migration runner executes directly on a `GET_LOCK`-pinned `PoolConnection<MySql>` without fake `"START TRANSACTION"` / `"COMMIT"` / `"ROLLBACK"` wrappers, with a `Why:` comment on implicit DDL auto-commit.
- [ ] All verification commands exit 0 with 0 warnings.
