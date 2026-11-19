# Task 10 — Deterministic Fixtures, Property Tests, CLI Template Alignment & `#![forbid(unsafe_code)]` (`RC-10`, `RC-11`)

## Goal

Make `TestTransactionFixture::drop` and `PostgresSchemaFixture::drop` execute cleanup synchronously without a `50ms` sleep, add `proptest` suites for `rume-enterprise`, `rume-data`, and `rume-foundation`, align `rume new` SQL templates and `--dialect` handling with `rume sql:check`, and add `#![forbid(unsafe_code)]` to `crates/macro` and `crates/xtask`.

## Files touched

- `Cargo.toml` (`[workspace.dependencies]`: add `proptest = "=1.12.0"`)
- `crates/foundation/Cargo.toml`, `crates/data/Cargo.toml`, `crates/enterprise/Cargo.toml`, `crates/core/Cargo.toml` (`[dev-dependencies]`: `proptest.workspace = true`)
- `crates/testing/src/fixtures.rs` (`TestTransactionFixture::drop`, `PostgresSchemaFixture::drop`, remove `sleep(50ms)` at `:355`)
- `crates/enterprise/src/enterprise/saml.rs` (`proptest` module for `SamlParser` and `chrono`-backed `parse_iso8601_strict`)
- `crates/data/src/persistence/{backend,sqlite}.rs` (`proptest` suites for `sqlparser`-backed `validate_managed_transaction_sql` and `reject_unbound_sqlite_placeholders`)
- `crates/foundation/src/env/interpolation.rs` (`proptest` suite for `interpolate_value`)
- `crates/cli/src/cli.rs` (`NewProjectDialect` `ValueEnum` and `--dialect` on `Commands::New`)
- `crates/cli/src/commands/mod.rs` (pass `dialect` from `Commands::New` to `new::run`)
- `crates/cli/src/commands/new.rs` (`run`, `run_inner`, unit test `run_inner_already_exists_does_not_delete` at line 318)
- `crates/cli/templates/sql/items/{create,find}.sql.tpl` (`?` placeholder default / dialect parameterization)
- `crates/cli/templates/README.md.tpl` (document `rume sql:check` and dialect placeholders / `sql/<dialect>/` subdirs)
- `crates/cli/tests/generated_project.rs` (verify `rume sql:check` on default scaffold and `--dialect postgres` scaffold)
- `crates/macro/src/lib.rs` & `crates/xtask/src/main.rs` (add `#![forbid(unsafe_code)]`)

## Deliverable

1. **Deterministic fixture `Drop` (`crates/testing/src/fixtures.rs`):**
   - Replace the fire-and-forget `Handle::try_current().spawn(...)` in `PostgresSchemaFixture::drop` (`:280-296`) and `TestTransactionFixture<T>::drop` (`:70-81`) with reactor-safe synchronous cleanup (5s timeout):
     - **`PostgresSchemaFixture::drop`:** When `Handle::try_current()` is `RuntimeFlavor::MultiThread`, run `tokio::task::block_in_place(|| handle.block_on(tokio::time::timeout(timeout, admin.backend().execute(...))))`. When `RuntimeFlavor::CurrentThread` (all 26 `containers/postgres.rs` tests!) or outside Tokio, extract a cloned `DatabaseConfig` from `self.admin_pool.take()` (`max_connections = 1`, `min_connections = 0`) and spawn + `.join()` a helper OS thread running `Builder::new_current_thread().enable_all().build()`, which opens a **fresh** `PostgresBackend::connect(&drop_cfg)` bound to the helper thread's own Tokio I/O reactor (never touching `admin_pool`'s outer-reactor sockets across `.join()`), executes `DROP SCHEMA IF EXISTS "{schema}" CASCADE`, and closes it before returning.
     - **`TestTransactionFixture<T>::drop`:** Match `Handle::try_current()`:
       - `Ok(h) if h.runtime_flavor() == RuntimeFlavor::MultiThread`: run `tokio::task::block_in_place(|| h.block_on(tokio::time::timeout(timeout, tx.rollback())))`.
       - `Ok(_)` or `Err(_)` when `tx.dialect() == "sqlite"`: run `tx.rollback().await` on a joined `std::thread::spawn` + `Builder::new_current_thread().enable_all().build()` (safe because `sqlx-sqlite` runs queries over channels on its own OS worker thread, not Tokio's socket reactor).
       - `Ok(h)` on `RuntimeFlavor::CurrentThread` when `tx.dialect() != "sqlite"` (`postgres`, `mysql`): do NOT `.join()` a helper thread over an outer-reactor TCP socket (blocking the single test thread halts `epoll_wait` and deadlocks); keep `h.spawn(async move { let _ = tx.rollback().await; })` and document on `TestTransactionFixture` (`///`) and `PostgresSchemaFixture` (`///`) that synchronous drop rollback barriers for Postgres/MySQL transactions require `#[tokio::test(flavor = "multi_thread")]` or explicit `.rollback().await`.
   - Delete `tokio::time::sleep(std::time::Duration::from_millis(50)).await;` at `fixtures.rs:355` so `test_transaction_fixture_auto_rollback_on_drop` proves determinism directly on `drop(tx)`.
2. **Property-based test suites (`RC-10`):**
   - Pin `proptest = "=1.12.0"` in root `Cargo.toml` `[workspace.dependencies]` and add `proptest.workspace = true` to `[dev-dependencies]` of `rume-foundation`, `rume-data`, `rume-enterprise`, and `rume`.
   - `rume-enterprise` (`crates/enterprise/src/enterprise/saml.rs`): proptests verifying `SamlParser::parse_unverified` and `chrono`-backed `parse_iso8601_strict` never panic on arbitrary UTF-8 strings (including 4-byte non-ASCII offsets), valid RFC 3339 instants with `Z` / `±HH:MM` (`00..=23` & `00..=59`) convert accurately to UTC epoch seconds, and out-of-range offsets, space separators, or pre-1970 timestamps fail closed.
   - `rume-data` (`crates/data/src/persistence/{backend,sqlite}.rs`): proptests verifying `sqlparser`-backed `validate_managed_transaction_sql` (across `PostgreSqlDialect`, `MySqlDialect`, `SQLiteDialect`) and `reject_unbound_sqlite_placeholders` never panic on arbitrary UTF-8/SQL fuzz inputs, always reject unquoted `COMMIT`/`ROLLBACK`/`BEGIN` statements (while permitting inside string literals/comments/delimited identifiers), and always reject unbound `?`/`:name`/`@name`/`$name` placeholders outside quotes/comments. *(Note in `README.md` § Flagged gaps: `Migrator::split_sql_statements` does not exist on `main` `31683f6`, so property coverage targets `rume-data`'s real SQL lexers.)*
   - `rume-foundation` (`crates/foundation/src/env/interpolation.rs`): proptests verifying `interpolate_value` never panics on arbitrary inputs, `\${VAR}` escapes round-trip to literal `${VAR}` regardless of `vars`, acyclic chains within `MAX_INTERPOLATION_DEPTH` resolve deterministically, and cyclic/too-deep chains fail closed without leaking variable values.
3. **CLI template & `--dialect` alignment (`RC-11`):**
   - Add `#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq)] pub enum NewProjectDialect { Sqlite, Postgres, Mysql }` in `crates/cli/src/cli.rs` and `--dialect` (`default_value_t = NewProjectDialect::Sqlite`) on `Commands::New` so invalid dialect values fail closed at CLI argument parsing; thread `dialect` through `crates/cli/src/commands/mod.rs:26-40`, `new::run` (`:128`), `new::run_inner` (`:173`), and unit test `run_inner_already_exists_does_not_delete` (`new.rs:318`).
   - Update `crates/cli/templates/sql/items/{create,find}.sql.tpl` to parameterize placeholders (`?` for `sqlite`/`mysql`, `$1`/`$2`/`$3` for `postgres`) so default `rume new` passes `rume sql:check`, `rume sql:check --dialect sqlite`, `rume sql:check --dialect mysql`, and `smoke.rs` (`SqliteTestFixture`) out of the box, while `rume new <name> --dialect postgres` passes `rume sql:check --dialect postgres` and `smoke.rs` (`SqliteTestFixture` executes `$N` natively while `sql/items/` keeps `dialect: None`).
   - Update `crates/cli/templates/README.md.tpl` to document `rume sql:check --dialect <sqlite|postgres|mysql>` and dialect subdirectories (`sql/<dialect>/...`), and extend `crates/cli/tests/generated_project.rs` to run `rume sql:check --dialect sqlite` on the default project and `rume sql:check --dialect postgres` on a `--dialect postgres` project.
4. **Workspace `#![forbid(unsafe_code)]`:** Add `#![forbid(unsafe_code)]` at the crate root of `crates/macro/src/lib.rs` and `crates/xtask/src/main.rs`.

## Constraints honoured

- `rules/10-tech-stack.md`: `proptest = "=1.12.0"` exact pin in root `Cargo.toml`.
- `rules/70-code-rationale.md`:
  - `Why:` comment on `PostgresSchemaFixture::drop` and `TestTransactionFixture::drop` in `crates/testing/src/fixtures.rs` explaining why outer-reactor Postgres/MySQL sockets cannot be polled by a joined helper thread on `RuntimeFlavor::CurrentThread` (blocking the sole test thread halts `epoll_wait`), why `PostgresSchemaFixture::drop` connects a fresh 1-connection `PostgresBackend` inside the helper thread on `current_thread` runtimes, and why `sqlx-sqlite` channel-based queries safely support a joined helper thread.
  - `Why:` comment in `crates/cli/src/commands/new.rs` on keeping `sql/items/*.sql` un-dialected while substituting `?` vs `$N` placeholders for `--dialect` (avoids `sql_assets!` picking a single dialect folder for flat `queries::items::*`, preserving `smoke.rs`'s `SqliteTestFixture` across dialects).
- `crates/cli/tests/generated_project.rs` & `crates/cli/tests/cli_workflows.rs` (all existing generator/CLI tests must keep passing).

## Named tests (bodies omitted)

- `test_transaction_fixture_auto_rollback_on_drop` & `test_transaction_fixture_multi_thread_sync_rollback` (`crates/testing/src/fixtures.rs` `#[cfg(test)]`) — retains existing assertions with `tokio::time::sleep` deleted, proving immediate deterministic rollback on `drop(tx)` on both `current_thread` (SQLite) and `multi_thread` (`block_in_place`) runtimes, plus a Postgres container check (`postgres_schema_fixture_drop_cleans_up_on_current_thread_runtime` in `crates/core/tests/containers/postgres.rs`) proving dropping `PostgresSchemaFixture` without `.close()` inside `#[tokio::test]` drops the schema synchronously without hanging.
- `saml_parser_and_iso8601_proptests` (`crates/enterprise/src/enterprise/saml.rs` `#[cfg(test)]`) — fuzzes `parse_unverified` and `chrono`-backed `parse_iso8601_strict` for panic freedom and strict RFC 3339 (`00..=23` / `00..=59`, non-negative epoch) bounds.
- `data_sql_scanners_proptests` (`crates/data/src/persistence/{backend,sqlite}.rs` `#[cfg(test)]`) — fuzzes `validate_managed_transaction_sql` and `reject_unbound_sqlite_placeholders` for panic freedom, comment/quote immunity, and fail-closed rejection.
- `interpolate_value_proptests` (`crates/foundation/src/env/interpolation.rs` `#[cfg(test)]`) — fuzzes arbitrary strings, escaped `\${VAR}`, and depth/cycle bounds.
- `new_project_passes_rume_sql_check_across_dialects` (`crates/cli/tests/generated_project.rs`) — scaffolds default (`sqlite`) and `--dialect postgres` projects and asserts `rume sql:check --dialect sqlite` / `--dialect postgres` and `cargo test` succeed.

## Verification

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -j 2 -- -D warnings
cargo test --workspace --lib --all-features -j 2
cargo test -p rume --test integration --all-features -j 2
cargo test -p rume-cli -j 2
cargo test -p rume-macros -j 2
cargo test --workspace --doc --all-features -j 2
```

Expected: 0 warnings, all workspace unit/integration/CLI/macro/doctests pass without sleeps or skipped assertions.

## Commit

Only when the user asks:

```text
fix(testing,cli): make fixture drop deterministic, add proptests, align SQL templates, and forbid unsafe workspace-wide
```

## Done-when

- [ ] `crates/testing/src/fixtures.rs` has no `tokio::time::sleep` and performs reactor-safe synchronous `Drop` cleanup (`PostgresSchemaFixture` via `block_in_place` or fresh-connection helper thread; `TestTransactionFixture` via `block_in_place` on `multi_thread` or joined helper thread for SQLite).
- [ ] `proptest = "=1.12.0"` is pinned in root `Cargo.toml`, and `proptest` suites exist and pass in `rume-enterprise`, `rume-data`, and `rume-foundation`.
- [ ] `rume new` rejects invalid `--dialect` values via `clap::ValueEnum`, scaffolds pass `rume sql:check --dialect sqlite` by default, and support `--dialect postgres`/`mysql`.
- [ ] Every crate under `crates/` (including `crates/macro/src/lib.rs` and `crates/xtask/src/main.rs`) declares `#![forbid(unsafe_code)]`.
- [ ] All verification commands exit 0 with 0 warnings.
