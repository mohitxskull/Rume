# 17-rc12-sql-check.md

## Goal

Fix RC-12 SQL validation defects in `crates/cli/src/commands/sql_check.rs`:

1. `$N` placeholder handling: the dialect check rejects `$N` for `"sqlite" | "mysql"` (line 321-324) — confirm intended semantics; per the audit the validator must not incorrectly reject valid placeholders or miss invalid ones per dialect (postgres uses `$N`, sqlite/mysql use `?`). Ensure each dialect only accepts its own placeholder style.
2. `EXPLAIN` with zero params: dry-run builds `EXPLAIN {trimmed}` for DML and executes it; with certain statements this fails. Fix the EXPLAIN dry-run to handle zero-parameter statements correctly (or skip EXPLAIN when there are no bindable params / use a valid explain form per dialect).
3. Comment-blind quote scanner: the quote/comment scanner (lines 210-272 and `contains_unquoted_char`/`contains_unquoted_dollar_param`) does not handle block comments (`/* ... */`) or `#` comments consistently, so a `?` or `$1` inside a comment is falsely flagged. Fix the scanner to skip block comments and line comments (`--`, `#`) when scanning for unquoted placeholders/quotes.

## Files touched

- `crates/cli/src/commands/sql_check.rs` — `validate_sql_file` dialect placeholder check, EXPLAIN dry-run, and the comment/quote scanner helpers.

## Constraints honoured

- Root cause, not symptom: make placeholder validation dialect-correct, comment-aware, and EXPLAIN robust to zero-param statements.
- No new dependencies, no `unsafe`.
- Record `Why:` comments for each scanner/placeholder decision.

## Done when (checkable by a command)

- `cargo test -p rume-cli sql_check` passes.
- `cargo clippy -p rume-cli --all-targets` and `cargo fmt --all --check` clean.
- A postgres file using `$1` passes; a sqlite/mysql file using `$1` is rejected; a sqlite/mysql file using `?` passes.
- `EXPLAIN` dry-run succeeds on a zero-param DML statement.
- A `?` or `$1` inside `--`/`#`/`/* */` comment is not flagged.

## Verification (fails-before test idea)

Tests:
1. **Comment-blind scanner:** `SELECT * FROM t -- ?` (postgres target) → **before fix** flagged as unquoted `?` → test fails; **after fix** passes (comment skipped).
2. **Block comment:** `/* ? */ SELECT 1` → not flagged.
3. **EXPLAIN zero-param:** a zero-param `SELECT 1` dry-run → **before fix** `EXPLAIN SELECT 1` may error → test fails; **after fix** handled → passes.
4. **Dialect placeholders:** postgres `$1` ok / `?` error; sqlite `?` ok / `$1` error.
