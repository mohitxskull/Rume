# Task 03 — Persistence Error Taxonomy, Lossless `usize` Conversion & Transaction SQL Guards (`RC-09`)

## Goal

Emit `PersistenceError::DecodeError` consistently across all `FromDbValue` implementations so `DbRow::{get, get_by_index, get_opt}` attach column context, preserve values above `i64::MAX` in `From<usize> for DbValue`, replace the hand-rolled SQL scanners (`validate_managed_transaction_sql` and `reject_unbound_sqlite_placeholders`) with `sqlparser = "=0.53.0"` across Postgres/MySQL/SQLite transactions, and deduplicate driver parameter-binding loops.

## Files touched

- `crates/data/Cargo.toml` (`sqlparser = { workspace = true, optional = true }` enabled by `sqlite`/`postgres`/`mysql`, plus `[dev-dependencies] sqlparser.workspace = true`)
- `crates/data/src/persistence/row.rs` (`FromDbValue` impls)
- `crates/data/src/persistence/backend.rs` (`From<usize> for DbValue`, `#[cfg(any(feature = "postgres", feature = "mysql", feature = "sqlite", test))] pub(crate) fn validate_managed_transaction_sql`)
- `crates/data/src/persistence/postgres.rs` (delete hand-rolled SQL scanner at `:580-729`, call `backend::validate_managed_transaction_sql` with `&PostgreSqlDialect {}`)
- `crates/data/src/persistence/mysql.rs` (call `validate_managed_transaction_sql` with `&MySqlDialect {}` in `MysqlTransaction::execute_params`, extract `bind_mysql_params`)
- `crates/data/src/persistence/sqlite.rs` (call `validate_managed_transaction_sql` with `&SQLiteDialect {}` in `SqliteTransaction::execute_params`, rewrite `reject_unbound_sqlite_placeholders` with `sqlparser`, extract `bind_sqlite_params`)

## Deliverable

1. **`crates/data/src/persistence/row.rs`:** Update every `FromDbValue` implementation (`String`, `i64`, `u64`, `f64`, `bool`, `Vec<u8>`) to return `PersistenceError::DecodeError { column: "<decode>".into(), message }` instead of `PersistenceError::QueryFailed(...)` so `DbRow::get`, `DbRow::get_by_index`, and `DbRow::get_opt` replace `column` with the real column name/index.
2. **`crates/data/src/persistence/backend.rs`:** Update `impl From<usize> for DbValue` (`:113-117`) to delegate to `Self::from(u as u64)` so `usize > i64::MAX` degrades losslessly to `DbValue::Text` instead of wrapping to negative `DbValue::Int`.
3. **Shared `sqlparser`-backed transaction SQL & placeholder guards + bind deduplication:**
   - Wire `sqlparser = { workspace = true, optional = true }` (`=0.53.0`, already pinned in root `Cargo.toml`) into `crates/data/Cargo.toml` (enabled under `sqlite`/`postgres`/`mysql` features so `--features redis,testing` stays light, plus `[dev-dependencies] sqlparser.workspace = true`).
   - Delete the hand-rolled character scanner (`parse_dollar_tag`, `check_pg_statement_keywords`, comment/quote state machine at `postgres.rs:580-729`), and move `validate_managed_transaction_sql(statement: &str, dialect: &dyn sqlparser::dialect::Dialect)` into `backend.rs` gated by `#[cfg(any(feature = "postgres", feature = "mysql", feature = "sqlite", test))]` (matching `map_sqlx_error` at `backend.rs:308` plus `test`). Tokenize with `sqlparser::tokenizer::Tokenizer::new(dialect, statement).tokenize()`, fail closed with `PersistenceError::TransactionError` on `TokenizerError`, ignore `Token::Whitespace(_)` (all comments `--`, MySQL `#`, and nested `/* ... */`), split statements on `Token::SemiColon`, and check each statement's leading unquoted `Token::Word` tokens (`word.quote_style.is_none()`) against `COMMIT`, `END`, `ABORT`, `BEGIN`, `START TRANSACTION`, `PREPARE TRANSACTION`, and `ROLLBACK` (allowing `ROLLBACK [WORK | TRANSACTION] TO ...`, string/dollar literals, and quoted/backtick/bracket identifiers natively).
   - Invoke `validate_managed_transaction_sql(&statement, dialect)?` in the empty-param `raw_sql` branch of `PostgresTransaction::execute_params` (`&PostgreSqlDialect {}`), `MysqlTransaction::execute_params` (`&MySqlDialect {}`), and `SqliteTransaction::execute_params` (`&SQLiteDialect {}`).
   - Rewrite `reject_unbound_sqlite_placeholders(statement: &str)` (`crates/data/src/persistence/sqlite.rs:610-691`) by deleting the manual byte loop and calling `Tokenizer::new(&SQLiteDialect {}, statement).tokenize_with_location()` (mapping `TokenizerError` to `PersistenceError::QueryFailed`): reject `Token::Placeholder(_)` (`?`, `?1`, `$name`) and any `Token::Colon` or `Token::AtSign` immediately adjacent (`tok.span.end == next.span.start`) to an unquoted `Token::Word` (`quote_style.is_none()`, plus `Token::Number` after `@`).
   - Extract `bind_mysql_params` in `mysql.rs` and `bind_sqlite_params` in `sqlite.rs` (mirroring `bind_pg_params` in `postgres.rs:47-62`) to replace all **8** duplicated `for param in &params_vec { match param { ... } }` loops in each file (4 in `*Backend` and 4 in `*Transaction`).

## Constraints honoured

- `rules/70-code-rationale.md`: `Why:` comments on (a) `From<usize> for DbValue` delegating via `u64` (lossless string fallback on 64-bit platforms without changing `DbValue::Int(i64)`), (b) `validate_managed_transaction_sql` & `reject_unbound_sqlite_placeholders` delegating SQL lexing to `sqlparser = "=0.53.0"` instead of maintaining hand-rolled dialect scanners (`#`, `[...]`, `` `...` ``, `$tag$...$tag$`, `E'...'`, nested `/* ... */`), and (c) `MysqlTransaction::execute_params` / `SqliteTransaction::execute_params` calling `validate_managed_transaction_sql` matching `PostgresTransaction::execute_params`.
- `crates/data/src/persistence/postgres.rs:731-775` (existing `validate_managed_transaction_sql` tests moved/kept in `backend.rs` tests) and `crates/data/src/persistence/sqlite.rs:698-752` (`sqlite_tx_rejects_unbound_placeholders_with_empty_params`).
- `crates/data/src/persistence/row.rs:347-423` (`test_column_not_found_in_get_and_get_opt`, `test_lossless_numeric_bounds_checking`) and `crates/core/tests/integration/sqlite.rs:497-530` (`QueryFailed` assertions on SQL/constraint errors remain `QueryFailed`).

## Named tests (bodies omitted)

- `from_db_value_errors_emit_decode_error_with_column_name` (`crates/data/src/persistence/row.rs` `#[cfg(test)]`) — tests NULL, invalid UTF-8 bytes, unparseable `i64`/`f64`/`bool`, and `Bytes`->numeric conversions through `DbRow::{get, get_by_index, get_opt}` and asserts `PersistenceError::DecodeError { column, .. }` carries the queried column name.
- `db_value_from_usize_handles_values_above_i64_max` (`crates/data/src/persistence/backend.rs` `#[cfg(test)]`) — asserts `DbValue::from(usize::MAX)` produces `DbValue::Text(usize::MAX.to_string())` rather than negative `DbValue::Int(-1)`.
- `validate_managed_transaction_sql_handles_all_dialects_comments_and_quotes` (`crates/data/src/persistence/backend.rs` `#[cfg(test)]`) — verifies ``SELECT `COMMIT` FROM t``, `# COMMIT\nSELECT 1;` (`MySqlDialect`), `SELECT [BEGIN] FROM t;` (`SQLiteDialect`), and `DO $$ BEGIN INSERT INTO t VALUES (1); END $$;` (`PostgreSqlDialect`) pass while raw `COMMIT` / `ROLLBACK` / `BEGIN` / `START TRANSACTION` fail, and `SqliteTransaction::execute_params` rejects raw `COMMIT` / `BEGIN` / `ROLLBACK` with `PersistenceError::TransactionError`.

## Verification

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -j 2 -- -D warnings
cargo check -p rume --no-default-features --features redis,testing -j 2
cargo test -p rume-data --all-features -j 2
cargo test -p rume --test integration --all-features sqlite -j 2
cargo test -p rume --test integration --all-features persistence -j 2
```

Expected: 0 warnings and all unit/integration tests pass.

## Commit

Only when the user asks:

```text
fix(persistence): standardize DecodeError taxonomy, lossless usize DbValue, and cross-driver tx guards
```

## Done-when

- [ ] `! rg -n 'PersistenceError::QueryFailed' crates/data/src/persistence/row.rs` matches only `get_value_by_index` out-of-bounds (no `FromDbValue` impl returns `QueryFailed`).
- [ ] `From<usize> for DbValue` delegates to `Self::from(u as u64)`.
- [ ] `validate_managed_transaction_sql` (in `backend.rs`) and `reject_unbound_sqlite_placeholders` (in `sqlite.rs`) delegate lexing to `sqlparser`, with `validate_managed_transaction_sql` enforced in Postgres, MySQL, and SQLite transaction `execute_params`.
- [ ] All verification commands exit 0 with 0 warnings.
