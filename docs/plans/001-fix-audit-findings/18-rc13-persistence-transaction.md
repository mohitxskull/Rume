# 18-rc13-persistence-transaction.md

## Goal

Fix RC-13 persistence `Transaction` error-handling inconsistency in `crates/core/src/persistence/postgres.rs`, `mysql.rs`, `sqlite.rs`:

1. Transaction methods (`execute`, `query_scalar`, `fetch_all`, etc. on the transaction handle) use `PersistenceError::QueryFailed(e.to_string())` directly in some places (~postgres.rs lines 390, 407, 439; sqlite.rs lines 417, 434, 466, 498, 530) while others use `map_sqlx_error` (~postgres.rs line 472, 504; sqlite.rs lines 172-331). This loses sqlx error classification (e.g. constraint violations vs connection errors). Fix: make all Transaction methods use `map_sqlx_error` consistently.
2. No `raw_sql` for empty params: the no-parameter `execute`/`query_scalar` paths lack a dedicated raw SQL entry point distinct from the parameterized variants, so callers cannot express "no params" explicitly. Fix: ensure a clean raw/no-param path exists (e.g. the existing no-param `execute`/`query_scalar` are the raw path, verified consistent with `map_sqlx_error`).

## Files touched

- `crates/core/src/persistence/postgres.rs` — Transaction impl error handling (~lines 380-510).
- `crates/core/src/persistence/mysql.rs` — Transaction impl error handling.
- `crates/core/src/persistence/sqlite.rs` — Transaction impl error handling (~lines 320-540).

## Constraints honoured

- Root cause, not symptom: consistent error classification so callers can match on `PersistenceError` variants correctly.
- No new dependencies, no `unsafe`.
- `map_sqlx_error` (backend.rs line 309) is the single conversion point for sqlx errors.
- Record `Why:` comment explaining consistent error mapping.

## Done when (checkable by a command)

- `cargo test -p rume-core persistence` passes.
- `cargo clippy -p rume-core --all-targets` and `cargo fmt --all --check` clean.
- Grep: all Transaction-method sqlx error conversions use `map_sqlx_error` (no `PersistenceError::QueryFailed(...to_string())` in transaction paths).
- A raw/no-param `execute` on a transaction works and maps errors via `map_sqlx_error`.

## Verification (fails-before test idea)

Tests:
1. **Consistency:** invoke a transaction method that triggers a sqlx error (e.g. a constraint violation or connection error) on each backend and assert the resulting `PersistenceError` is the `map_sqlx_error` classification (not a bare `QueryFailed`). **Before fix** some paths return bare `QueryFailed` → test fails; **after fix** all use `map_sqlx_error` → passes.
2. **Raw no-param SQL:** execute a no-parameter statement via the transaction raw path and assert success + correct error mapping on failure.
