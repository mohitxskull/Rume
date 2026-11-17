# 03-rc04-outbox-mark-failed-recursion-pool-acquire.md

## Goal

Fix RC-04 (repository/pool layer): two defects in outbox persistence.

1. `OutboxRepository` trait defaults for `mark_failed` and `mark_failed_with_max_recurse` recurse into each other (`mark_failed` → `mark_failed_with_max_retries(..., 5)` → `mark_failed` → …), causing infinite recursion / stack overflow when a backend overrides neither.
2. `DatabasePool`'s `OutboxRepository` impl uses the non-waiting `acquire_lease()` (which returns `Err(PoolExhausted)` immediately when capacity is exhausted) instead of a waiting acquire, so outbox transitions fail under load instead of waiting for a connection.

Fix root cause:
- Break the mutual default recursion: give each default method a standalone implementation (or have only one default and document that backends must override at least one), so they never call each other.
- Change `DatabasePool`'s outbox methods to use the waiting `acquire_lease_timeout(...)` (or otherwise block until a lease is available) instead of `acquire_lease()`.

## Files touched

- `crates/core/src/outbox/repository.rs` — `mark_failed` / `mark_failed_with_max_retries` default methods (lines 65-83).
- `crates/core/src/persistence/pool.rs` — `DatabasePool` `OutboxRepository` impl (lines 517-705), especially `save`, `claim_pending_with_lease`, `mark_failed*`, `renew_outbox_lease`, etc.

## Constraints honoured

- Root cause, not symptom: remove the recursive default, not just a depth guard.
- No new dependencies, no `unsafe`.
- Keep `acquire_lease_timeout` using `config.connect_timeout()` as the wait bound (consistent with the rest of `DatabasePool`).

## Done when (checkable by a command)

- `cargo test -p rume-core outbox` passes.
- `cargo clippy -p rume-core --all-targets` and `cargo fmt --all --check` clean.
- A unit test confirms calling the trait defaults on a backend that overrides neither does not recurse infinitely (bounded completion).
- `DatabasePool` outbox methods use `acquire_lease_timeout` (grep for `acquire_lease()` in pool.rs outbox impl — should be gone).

## Verification (fails-before test idea)

1. **Recursion:** construct a minimal `OutboxRepository` implementor that overrides neither `mark_failed` nor `mark_failed_with_max_retries`, call `mark_failed` with a timeout, and assert it returns within the timeout (does not stack-overflow). Before the fix it recurses until stack overflow / hang; after it returns an error or result bounded in time.
2. **Pool waiting:** under a saturated pool (semaphore exhausted), `claim_pending_with_lease` via `DatabasePool` should wait for a lease rather than immediately returning `PoolExhausted`. Assert the call blocks then succeeds once a lease is released.
