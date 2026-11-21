# Task 06 — Split `crates/data/src/persistence/backend.rs`

## Goal

Split `crates/data/src/persistence/backend.rs` (1,463 lines) into
`crates/data/src/persistence/backend/{mod,validation}.rs` — behavior-preserving;
`cargo build` + `cargo test -p rume-data` green.

## Files touched

- `crates/data/src/persistence/backend.rs` — deleted; replaced by:
- `crates/data/src/persistence/backend/mod.rs` — `BoxFuture` (backend.rs:11), `DbValue` (22-169,
  all `From` impls), the `params!` macro (172-181), `DatabaseBackend` trait (184-329),
  `DatabaseTransaction` trait (995-1059), `map_sqlx_error` (332-339), `#[cfg(feature = "fuzzing")]
  pub use validation::fuzz_support;`, `pub(crate) use validation::{validate_managed_transaction_sql,
  validate_managed_mysql_transaction_sql, mysql_sql_causes_implicit_commit,
  sanitize_non_nested_block_comments};` — this `pub(crate) use` bridge preserves the
  `crate::persistence::backend::X` paths used by callers; `mod tests` (1062-1194:
  `db_value_from_usize_handles_values_above_i64_max`, `map_sqlx_error_preserves_driver_classifications`).
- `crates/data/src/persistence/backend/validation.rs` — the SQL-safety validation machinery:
  `check_managed_tx_control_keywords` (342), `sanitize_non_nested_block_comments` (+`_inner`)
  (379/395), `tokenize_sql_filtered` (521), `collect_top_level_statement_heads_from_tokens` (559),
  `check_ast_tx_control` (640), `is_mysql_implicit_commit_ast` (659), `InheritedSetScope` (754),
  `evaluate_mysql_set_lhs` (773), `is_mysql_implicit_commit_head` (735),
  `check_mysql_autocommit_assignment_tokens` (835), `validate_managed_transaction_sql` (710),
  `mysql_sql_causes_implicit_commit` (906), `validate_managed_mysql_transaction_sql` (937),
  `fuzz_support` (977); `mod data_sql_scanners_proptests` (1197-1285).

## Deliverable

`crate::persistence::backend::{DbValue, DatabaseBackend, DatabaseTransaction, BoxFuture,
map_sqlx_error, validate_managed_transaction_sql, validate_managed_mysql_transaction_sql,
mysql_sql_causes_implicit_commit, sanitize_non_nested_block_comments, fuzz_support}` — all
resolve through `backend/mod.rs` as before (callers in `crates/data/src/persistence/{postgres,mysql,sqlite}.rs`,
`crates/data/src/persistence/migration/runner.rs` unchanged). `rume_data::params!` (macro_export,
crate-root path) unchanged.

## Constraints honoured

- `crate::persistence::backend::X` call paths for the validation fns:
  `crates/data/src/persistence/mysql.rs:357,391,414,434` (`validate_managed_mysql_transaction_sql`),
  `crates/data/src/persistence/postgres.rs:362,396,426,449` (`validate_managed_transaction_sql`),
  `crates/data/src/persistence/sqlite.rs:394,429,455,478` (`validate_managed_transaction_sql`) and
  `:616` (`sanitize_non_nested_block_comments`), `crates/data/src/persistence/migration/runner.rs`
  (`mysql_sql_causes_implicit_commit`) — unchanged via the mod.rs re-export bridge.
- No behavior change; `cargo build` + `cargo test -p rume-data` green.

## Named tests (bodies omitted — moved from `crates/data/src/persistence/backend.rs:1062-1324`)

From `backend/mod.rs`:
- `db_value_from_usize_handles_values_above_i64_max` — asserts `DbValue::from(usize::MAX)` is `Text`
  on 64-bit (degrades losslessly).
- `map_sqlx_error_preserves_driver_classifications` — asserts `map_sqlx_error` maps `PoolTimedOut`,
  `PoolClosed`, `Io`, and other sqlx errors to the `Timeout`/`Closed`/`ConnectionFailed`/`QueryFailed`
  `PersistenceError` variants.

From `backend/validation.rs` (`mod tests` + `mod data_sql_scanners_proptests`):
- `rejects_transaction_control_statements_in_raw_batches` — asserts `validate_managed_transaction_sql`
  rejects raw `COMMIT`/`ROLLBACK`/`BEGIN`/`START TRANSACTION`/`END`/`ABORT`/`PREPARE TRANSACTION`
  across the three dialects.
- `allows_non_resolving_batches_and_quoted_or_commented_keywords` — asserts keywords inside quotes,
  line/block comments, dollar-quoted blocks, savepoints, and compound bodies pass.
- `validate_managed_transaction_sql_handles_all_dialects_comments_and_quotes` — cross-dialect
  comment/quote handling (backtick, `#`, bracket, dollar-quote, Unicode-safe trimming).
- `validate_managed_transaction_sql_never_panics_on_arbitrary_utf8` (proptest).
- `validate_managed_transaction_sql_always_rejects_unquoted_tx_control` (proptest — case variants + suffixes).
- `validate_managed_transaction_sql_permits_keywords_in_literals_comments_and_delimited_ids` (proptest).
- `validate_managed_transaction_sql_allows_compound_bodies_and_rejects_trailing_tx_control` — asserts
  SQLite `CREATE TRIGGER` bodies and PostgreSQL `BEGIN ATOMIC` function bodies are allowed, and
  trailing `COMMIT`/`ROLLBACK` after them are rejected.

## Verification

```
cargo build
cargo test -p rume-data
```

## Commit

Only when the user asks:

```
refactor(data): split persistence/backend.rs into backend/ mod and validation modules
```

## Done-when

- [ ] `crates/data/src/persistence/backend/` contains `mod.rs` + `validation.rs`;
  `crates/data/src/persistence/backend.rs` deleted.
- [ ] `crate::persistence::backend::{DbValue, DatabaseBackend, DatabaseTransaction, BoxFuture,
  map_sqlx_error, validate_managed_transaction_sql, validate_managed_mysql_transaction_sql,
  mysql_sql_causes_implicit_commit, sanitize_non_nested_block_comments, fuzz_support}` resolve.
- [ ] `cargo build` + `cargo test -p rume-data` green.
