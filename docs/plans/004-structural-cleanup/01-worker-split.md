# Task 01 — Split `crates/messaging/src/jobs/worker.rs`

## Goal

Split `crates/messaging/src/jobs/worker.rs` (3,286 lines) into a `worker/` module directory:
behavior-preserving, `cargo build` + `cargo test -p rume-messaging` green after the task.

## Files touched

- `crates/messaging/src/jobs/worker.rs` — deleted; replaced by:
- `crates/messaging/src/jobs/worker/mod.rs` — module docs; `mod heartbeat; mod registry; mod runner;`;
  re-exports `pub use registry::JobRegistry; pub use runner::{JobWorker, WorkerStatus};`;
  `WorkerHandle` + `await_supervised_task` (moved from worker.rs:148-356);
  `#[cfg(test)] mod tests;` → `worker/tests.rs`.
- `crates/messaging/src/jobs/worker/registry.rs` — `ErasedHandler` (worker.rs:21), `JobRegistry`
  (26-120, `register`/`dispatch`/`dispatch_with_cancellation` + Debug), `InFlightGuard` (123-129).
- `crates/messaging/src/jobs/worker/runner.rs` — `ActiveTaskGuard` (174-187), `JobWorker` struct +
  `Clone`/`Debug` impls (359-426) and `impl<Q: JobQueue> JobWorker` (428-1231):
  `DEFAULT_CONCURRENCY`, `new`, `with_concurrency_limit`, `with_concurrency`, `with_worker_id`,
  `with_heartbeat`, `with_abandoned_recovery`, `with_idempotency_store`, `worker_id`, `status`,
  `transition_errors`, `lost_leases`, `start` (540-625), `record_join_set_len`,
  `log_in_flight_outcome`, `sleep_while_reaping`, `run_loop` (671-826),
  `execute_job` (829-1200, marked `pub(super)` so `worker::tests` can call it), and the `JobWorker::execute_job` heartbeat
  plumbing.
- `crates/messaging/src/jobs/worker/heartbeat.rs` — the per-job lease-heartbeat spawn (worker.rs:921-1030),
  `HeartbeatAbortGuard` (1031-1037), `release_idempotency_token` (1235-1263).
- `crates/messaging/src/jobs/worker/tests.rs` — the `#[cfg(test)] mod tests` moved verbatim
  (worker.rs:1265-3286).

Visibility decision: `WorkerHandle` fields stay private and live in `worker/mod.rs`, so
`worker::tests` (a descendant of `worker`) can construct `WorkerHandle` with the same field sets
it uses today; `JobWorker::execute_job` becomes `pub(super)` for the same reason.

## Deliverable

`rume::jobs::{JobRegistry, JobWorker, WorkerHandle, WorkerStatus}` resolve via
`jobs/mod.rs:23` `pub use worker::{JobRegistry, JobWorker, WorkerHandle, WorkerStatus};`
(unchanged); every item in `worker.rs` appears with identical bodies in its new home; no
behavior changes.

## Constraints honoured

- No behavior change: fns move as-is; no signature/semantic edits (only visibility of
  `JobWorker::execute_job` widened to `pub(super)`).
- `crates/messaging/src/jobs/mod.rs:23` unchanged.
- Core tests using `rume::jobs::{JobRegistry, JobWorker, …}` (`crates/core/tests/containers/*`,
  `crates/core/tests/integration/*`) unchanged.

## Named tests (bodies omitted — moved from `crates/messaging/src/jobs/worker.rs:1265+`)

From `worker/tests.rs` (worker.rs:1265+):
- `worker_drain_while_dequeue_blocked_does_not_start_job_and_nacks_immediately` — asserts drain during
  a blocked dequeue returns `true`, the job never executes, and the unstarted job is nacked once.
- `test_worker_handle_shutdown_reports_panic` — asserts `WorkerHandle::shutdown` returns `false` when
  the worker task panicked.
- `test_worker_handle_timeout_aborts_tasks` — asserts `try_shutdown` returns `false` on timeout and the
  long-running task is aborted after `shutdown` times out.
- `test_worker_handle_try_shutdown_retry_after_heartbeat_timeout` — asserts `try_shutdown` returns `false`
  while the heartbeat task is unfinished, then `true` on retry.
- `test_purge_completed_queue_api` — asserts `purge_completed(3600s)` returns 5 via `BlockingTestQueue`.
- `test_worker_heartbeat_loss_routes_to_dlq` — asserts a lost lease routes the completed job to DLQ
  (not acked).
- `test_idempotency_store_acquisition_failure_fails_closed_and_nacks` — asserts `nack` 1, `ack` 0 when
  idempotency acquisition fails.
- `test_worker_idempotency_renewal_failure_routes_to_dlq` / `test_worker_default_unsupported_idempotency_renewal_routes_to_dlq`
  — assert DLQ 1, `ack` 0 on renewal failure / on stores without renewal support.
- `test_worker_retryable_failure_releases_idempotency_lock_before_nack` — asserts `release_with_token`
  is logged before `nack` and keyed by `scoped_idempotency_key("tenant-a", "idemp-key")`.
- `test_worker_lease_lost_releases_only_after_dead_letter_succeeds` — asserts `release_with_token`
  happens only after successful `dead_letter`; `transition_errors == 1` when DLQ fails.
- `test_worker_keeps_heartbeat_active_until_nack_completes_on_slow_release` — asserts `renew_count >= 2`
  while `release_with_token` is slow; release precedes nack.
- `test_worker_in_progress_idempotency_defers_without_burning_retry_budget` — asserts `defer` without
  consuming retry attempts.
- `worker_fast_completing_job_never_leaks_abort_handle` — asserts no abort-handle leak for fast jobs.
- `in_flight_idempotency_loss_before_ack_routes_to_dead_letter` — asserts idempotency loss before ack
  routes to DLQ.
- `stalled_heartbeat_renewal_times_out_and_does_not_hang_finalization` — asserts renewal timeout doesn't
  hang finalization.

## Verification

```
cargo build
cargo test -p rume-messaging
```

## Commit

Only when the user asks:

```
refactor(messaging/jobs): split worker.rs into a worker/ module tree
```

## Done-when

- [ ] `crates/messaging/src/jobs/worker.rs` no longer exists; `crates/messaging/src/jobs/worker/`
  contains `mod.rs`, `registry.rs`, `runner.rs`, `heartbeat.rs`, `tests.rs`.
- [ ] `rume::jobs::{JobRegistry, JobWorker, WorkerHandle, WorkerStatus}` still resolve (via
  `jobs/mod.rs:23` re-exports).
- [ ] `cargo build` and the full `cargo test -p rume-messaging` suite green.
