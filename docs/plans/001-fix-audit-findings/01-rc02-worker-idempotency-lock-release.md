# 01-rc02-worker-idempotency-lock-release.md

## Goal

Fix RC-02: in `worker.rs` the retryable-failure branch (`execute_job`, ~lines 837-851) calls `queue.nack(job, &token, backoff)` without first releasing the distributed idempotency lock. The lock (held via `try_acquire_with_token` with a 300s TTL) is never released for a job that will be retried, so the same idempotency key stays owned by this worker across retry attempts — blocking other workers from ever deduping/re-executing that key until the TTL expires.

Fix root cause: release the idempotency lock (`store.release_with_token(key, &token)`) **before** `queue.nack()` on retryable errors. Do NOT key the lock by `job.id` (the lock key is already the scoped idempotency key `effective_key`, computed at lines 630-634; keep it that way).

## Files touched

- `crates/core/src/jobs/worker.rs` — `execute_job` retryable branch (~lines 837-851).

## Constraints honoured

- Root cause, not symptom: release the lock before nacking so retry ownership is relinquished.
- Do NOT key the lock by `job.id`; continue using `effective_key` (tenant-scoped idempotency key).
- No new dependencies, no `unsafe`.
- Record a `Why:` comment at the release site explaining that lock ownership is retained only until the terminal transition (success/ack or terminal dead-letter).

## Done when (checkable by a command)

- `cargo test -p rume-core jobs::worker` passes.
- `cargo clippy -p rume-core --all-targets` and `cargo fmt --all --check` are clean.
- A grep confirms the retryable branch now calls `store.release_with_token(key, &token)` before `queue.nack(job, &token, backoff)` and still does not use `job.id` as a lock key.

## Verification (fails-before test idea)

Add a test in `worker.rs`'s `mod tests` using a fake `IdempotencyStore` that records `release_with_token` calls and a fake queue that records `nack` calls:

1. **Before fix:** a retryable job error → `nack` is called but `release_with_token` is NOT called (lock leaked). The test asserts `release_with_token` was called before `nack` and fails before the fix.
2. **After fix:** `release_with_token(key, &token)` is invoked (with the scoped key, not `job.id`) ahead of `queue.nack(job, &token, backoff)`; the test passes.

Ordering check: capture a sequence log in the fakes to assert release precedes nack.
