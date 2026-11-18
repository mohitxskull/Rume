# 02-rc04-outbox-claim-lease-fetch-failed-sql.md

## Goal

Fix RC-04 (SQL layer): all three `outbox_claim_lease.sql` dialects (postgres, mysql, sqlite) lack `AND status = 'pending'`, so a claim can transition an already-claimed/inflight message and double-dispatch or lose the lease. Also `outbox_fetch_failed.sql` (all three dialects) omits the `trace_context` column, so failed-message inspection loses trace context.

Fix root cause:
- Add `AND status = 'pending'` to the `WHERE` clause of `outbox_claim_lease.sql` for postgres, mysql, and sqlite so only pending messages can be claimed.
- Add `trace_context` to the `SELECT` column list of `outbox_fetch_failed.sql` for all three dialects (the `OutboxMessage` struct has a `trace_context: Option<String>` field).

## Files touched

- `crates/core/src/persistence/sql/postgres/outbox_claim_lease.sql`
- `crates/core/src/persistence/sql/mysql/outbox_claim_lease.sql`
- `crates/core/src/persistence/sql/sqlite/outbox_claim_lease.sql`
- `crates/core/src/persistence/sql/postgres/outbox_fetch_failed.sql`
- `crates/core/src/persistence/sql/mysql/outbox_fetch_failed.sql`
- `crates/core/src/persistence/sql/sqlite/outbox_fetch_failed.sql`

## Constraints honoured

- Root cause, not symptom: the claim must be gated on `status = 'pending'` at the SQL level, not filtered in application code.
- No new dependencies, no `unsafe`.
- Keep placeholder style per dialect (`$1` postgres, `?` mysql, `?1`/`?` sqlite) — do not introduce cross-dialect placeholder mismatches.

## Done when (checkable by a command)

- `cargo test -p rume-core` passes (includes outbox integration tests).
- `cargo clippy -p rume-core --all-targets` and `cargo fmt --all --check` clean.
- Grep each claim_lease.sql confirms `AND status = 'pending'` present; grep each fetch_failed.sql confirms `trace_context` in the SELECT list.

## Verification (fails-before test idea)

Add an integration test (`crates/core/tests/integration/outbox.rs` or the outbox unit tests) that:

1. Inserts a message, claims it (status → inflight), then attempts a second claim. **Before fix** the second claim succeeds (no `status = 'pending` guard) → test fails; **after fix** the second claim returns no rows → test passes.
2. Fetches failed messages and asserts the returned `OutboxMessage.trace_context` matches the inserted value. **Before fix** the column is missing/None → test fails; **after fix** it round-trips → test passes.
