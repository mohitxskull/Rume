# Task 14 — Split `crates/testing/src/fixtures.rs` into area modules

## Goal

Split `crates/testing/src/fixtures.rs` (777 lines) into per-area modules under
`crates/testing/src/fixtures/` — behavior-preserving; `cargo build` +
`cargo test -p rume-testing --features sqlite` green.

## Files touched

- `crates/testing/src/fixtures.rs` — deleted; replaced by:
- `crates/testing/src/fixtures/mod.rs` — module doc (fixtures.rs:1); re-exports
  `pub use transaction::TestTransactionFixture;` + `#[cfg(feature = "sqlite")] pub use sqlite::SqliteTestFixture;`
  + `#[cfg(feature = "postgres")] pub use postgres::PostgresSchemaFixture;`;
  `#[cfg(all(test, feature = "sqlite"))] mod tests;` → `fixtures/tests.rs`.
- `crates/testing/src/fixtures/transaction.rs` — `TestTransactionFixture<T>` struct (fixtures.rs:20-25),
  `impl` block (27-36), `with_pending_rollouts` → renamed to `with_pending_rollbacks` (39-48,
  `pub(super)` — called by `fixtures/postgres.rs`), `tx` (51-53), `commit` (56-62), `rollback`
  (65-70), `close` (73-75), `Deref` (78-83), `Debug` (85-91), `Drop` (93-155).
- `crates/testing/src/fixtures/sqlite.rs` — `SqliteTestFixture` struct (159-163) + impls: `create`
  (168-183), `pool` (185-188), `path` (190-193), `begin_test_tx` (195-202), `close` (204-224),
  `Deref` (228-233), `Debug` (236-243), `Drop` (246-255); imports `use super::TestTransactionFixture;`.
- `crates/testing/src/fixtures/postgres.rs` — `PostgresSchemaFixture` struct (269-275) + impls:
  `create` (281-313), `schema_name` (315-318), `pool` (321-323), `begin_test_tx` (326-335),
  `close` (338-356), `Deref` (360-365), `Debug` (368-375), `Drop` (378-451);
  `begin_test_tx` calls `TestTransactionFixture::with_pending_rollbacks` (pub(super) in transaction.rs).
- `crates/testing/src/fixtures/tests.rs` — the `#[cfg(all(test, feature = "sqlite"))] mod tests`
  moved verbatim (fixtures.rs:453-777, incl. `GatedRollbackTx`).
- `crates/testing/src/lib.rs` — **unchanged** (`pub mod fixtures;` + the re-export list
  (`pub use fixtures::{PostgresSchemaFixture, SqliteTestFixture, TestTransactionFixture};`) resolve
  through the new `fixtures/mod.rs` with identical cfg gates).

Visibility notes: `fixtures/sqlite.rs` uses `TestTransactionFixture` via `use super::TestTransactionFixture;`
(mod.rs re-export); `fixtures/tests.rs` uses `use super::*` (sees mod.rs re-exports).

## Deliverable

- `rume_testing::fixtures::{TestTransactionFixture, SqliteTestFixture, PostgresSchemaFixture}` and
  the lib-level re-exports (`rume_testing::{TestTransactionFixture, SqliteTestFixture, PostgresSchemaFixture}`)
  resolve as before.
- Area layout: `transaction` (TestTransactionFixture), `sqlite` (SqliteTestFixture),
  `postgres` (PostgresSchemaFixture), plus the moved test module at `fixtures/tests.rs`.

## Constraints honoured

- `crates/testing/src/lib.rs` unchanged — `pub mod fixtures;`, `pub use fixtures::PostgresSchemaFixture;`
  (`#[cfg(feature = "postgres")]`), `pub use fixtures::SqliteTestFixture;` (`#[cfg(feature = "sqlite")]`),
  `pub use fixtures::TestTransactionFixture;`.
- No behavior change; `cargo build` + `cargo test -p rume-testing --features sqlite` green.
- Global constraints as above.

## Named tests (bodies omitted — moved from `crates/testing/src/fixtures.rs:453-777`)

From `fixtures/tests.rs`:
- `test_sqlite_test_fixture_creates_and_cleans_up` — asserts the temp db file exists, `CREATE TABLE`/`INSERT`/`COUNT` work, and the file is removed after `drop`.
- `test_transaction_fixture_auto_rollback_on_drop` — asserts an uncommitted tx inserted but dropped is rolled back (count 0), and a committed tx persists (count 1).
- `test_sqlite_test_fixture_explicit_close_and_cleanup` — asserts `close` removes the db file and `-wal`/`-shm` sidecars.
- `test_transaction_fixture_explicit_rollback` — asserts explicit `rollback()` leaves nothing behind.
- `test_parallel_fixtures_deterministic_isolation` — spawns 5 fixtures in parallel, asserts each sees only its own row (count 1).
- `test_transaction_fixture_multi_thread_sync_rollback` — uses `GatedRollbackTx` + `TestTransactionFixture` + `SqliteTestFixture`; asserts multi-thread drop synchronously completes `rollback()` before returning, and the real SQLite data is rolled back.

## Verification

```
cargo build
cargo test -p rume-testing --features sqlite
```

## Commit

Only when the user asks:

```
refactor(rume-testing): split fixtures.rs into fixtures/ area modules
```

## Done-when

- [ ] `crates/testing/src/fixtures/` contains `mod.rs`, `transaction.rs`, `sqlite.rs`, `postgres.rs`, `tests.rs`;
  `crates/testing/src/fixtures.rs` deleted.
- [ ] `rume_testing::fixtures::{TestTransactionFixture, SqliteTestFixture, PostgresSchemaFixture}` and
  `rume_testing::{TestTransactionFixture, SqliteTestFixture, PostgresSchemaFixture}` resolve
  (via unchanged `lib.rs`).
- [ ] `cargo build` + `cargo test -p rume-testing --features sqlite` green.
