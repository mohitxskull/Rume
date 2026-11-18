# 17-rc12-sql-check.md

## OVERRIDE — Dependency-based approach
Do NOT hand-roll the SQL quote/comment scanner. Instead:
- Add the `sqlparser-rs` crate to workspace dependencies and cli Cargo.toml
- Rewrite `crates/cli/src/commands/sql_check.rs` to use `sqlparser-rs` for dialect-aware placeholder validation, comment-aware scanning, and EXPLAIN dry-run validation
- `sqlparser-rs` handles block comments (`/* ... */`), line comments (`--`), `#` comments, and proper placeholder validation per dialect

## Task file
Read the full task file: /home/skull/Projects/rume/docs/plans/001-fix-audit-findings/17-rc12-sql-check.md

## Your job
Fix RC-12 sql_check defects using sqlparser-rs:

1. Add `sqlparser-rs` crate to workspace `Cargo.toml` (`[workspace.dependencies]`) and `crates/cli/Cargo.toml`
2. Replace hand-rolled `content.chars()` quote/comment scanner in `crates/cli/src/commands/sql_check.rs` with `sqlparser-rs` parsing
3. Fix dialect placeholder validation: postgres uses `$N`, sqlite/mysql use `?` — sqlparser handles this natively
4. Fix EXPLAIN dry-run to handle zero-parameter statements correctly via sqlparser validation
5. Comments (`--`, `#`, `/* */`) are properly skipped so `?` or `$1` inside comments is not falsely flagged

## Files touched
- `Cargo.toml` (workspace deps)
- `crates/cli/Cargo.toml` (add sqlparser-rs)
- `crates/cli/src/commands/sql_check.rs` — rewrite using sqlparser-rs

## Constraints
- Use sqlparser-rs for SQL parsing and validation
- No `unsafe`
- Record `Why:` comments at decision sites
- postgres file using `$1` passes; sqlite/mysql file using `$1` rejected; sqlite/mysql file using `?` passes
- `?` or `$1` inside `--`/`#`/`/* */` comment is not flagged
- EXPLAIN dry-run succeeds on zero-param DML

## Verification
1. `cargo test -p rume-cli sql_check`
2. `cargo clippy -p rume-cli --all-targets`
3. `cargo fmt --all --check`

## Handoff
Write to `/home/skull/Projects/rume/docs/plans/001-fix-audit-findings/scratch/task-17-done.md`. Do not commit.
