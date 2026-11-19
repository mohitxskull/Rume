# Task 01 — Concurrency & RAII Singleflight / Worker Handle Synchronization (`RC-01`)

## Goal

Eliminate leaked singleflight slots and stampede fall-throughs in `Cache::remember_with_tags`, and replace manual `JobWorker` `in_flight_handles` / `in_flight_seq` / `InFlightHandleGuard` bookkeeping with `tokio::task::JoinSet` so task spawns cannot race handle registration.

## Files touched

- `crates/data/src/cache/typed.rs` (`Cache::singleflight`, `Cache::remember_with_tags`, new `SingleflightLeaderGuard`)
- `crates/messaging/src/jobs/worker.rs` (`JobWorker`, `JobWorker::run_loop`, `WorkerHandle`, remove `in_flight_handles`, `next_job_seq`, `InFlightHandleGuard`, and update test literals)

## Deliverable

1. **`crates/data/src/cache/typed.rs`:**
   - Replace `tokio::sync::Mutex` on `Cache::singleflight` with `std::sync::Mutex` (never held across `.await`) so synchronous `Drop` unconditionally cleans up slots without failing `try_lock()`.
   - Introduce an RAII `SingleflightLeaderGuard` holding `singleflight`, `key`, and its `Arc<tokio::sync::Notify>` that on `Drop` removes the entry only if `Arc::ptr_eq` matches its own `Notify` and always invokes `notify.notify_waiters()` — guaranteeing cleanup and wakeup across `f().await` errors, panics, and caller future cancellation.
   - Wrap follower waiting in `remember_with_tags` inside a `loop`: acquire `singleflight` lock; if a slot exists, clone `Arc<Notify>` and register `notified = notify.notified()` before unlocking, `notified.await`, check `self.get_json::<T>(key).await?`, and if `None` (previous leader failed, panicked, or was cancelled), `continue` to re-contend for leadership rather than executing `f().await` unguarded. Only the task that inserts the `Notify` and constructs `SingleflightLeaderGuard` exits the loop and runs `f().await` once.
2. **`crates/messaging/src/jobs/worker.rs` (`tokio::task::JoinSet`):**
   - Delete `in_flight_handles: Arc<Mutex<HashMap<u64, AbortHandle>>>`, `next_job_seq`, and `InFlightHandleGuard` from `JobWorker` (`:358,382,440`), `WorkerHandle` (`:176-177,207-211,596`), and test struct literals (`:1301,1335,1377`).
   - Own a `let mut in_flight_tasks = tokio::task::JoinSet::<()>::new();` directly inside `JobWorker::run_loop` (`:604`), spawn each dequeued job via `in_flight_tasks.spawn(...)`, reap completed tasks via `in_flight_tasks.try_join_next()` / `tokio::select! { res = in_flight_tasks.join_next(), if !in_flight_tasks.is_empty() => ... }` (logging unexpected task panics), drain active tasks on graceful shutdown (`while in_flight_tasks.join_next().await.is_some() {}`), and call `in_flight_tasks.abort_all()` (via shutdown/abort signal or `JoinSet::drop` when `WorkerHandle::abort_all` aborts `self.worker_task` on shutdown timeout).

## Constraints honoured

- `crates/data/src/cache/typed.rs:23,149-211` (`Cache::remember_with_tags` signature, `FnOnce() -> Fut` bound — `f` is moved only after winning leadership).
- `crates/core/tests/integration/cache.rs:197` (`cache_remember_stampede_singleflight_protection`) and `crates/core/tests/integration/concurrency_invariants.rs:127` (`singleflight_stampede_protection_interleaving_executes_closure_once`).
- `crates/messaging/src/jobs/worker.rs:197-304,1282-1387` (`WorkerHandle::{try_shutdown, shutdown, abort_all}` semantics preserved; `abort_all` aborts `worker_task` / drops `JoinSet`).
- `rules/70-code-rationale.md`: Record `Why:` comments on (a) `Cache::singleflight` using `std::sync::Mutex` + `Arc::ptr_eq` guard in `SingleflightLeaderGuard::drop` (alternative `tokio::sync::Mutex` cannot lock reliably in sync `Drop`, and `try_lock` could drop wakeups if contended; revisit if async `Drop` stabilizes), and (b) `JobWorker::run_loop` using `tokio::task::JoinSet` instead of manual `Mutex<HashMap<u64, AbortHandle>>` + `InFlightHandleGuard` (eliminates spawn-before-insert races and mutex overhead by construction, reaps finished `JoinHandle`s automatically, and aborts all child tasks via `abort_all()` / `JoinSet::drop` on shutdown timeout).

## Named tests (bodies omitted)

- `singleflight_leader_cancellation_wakes_follower_and_cleans_slot` (`crates/data/src/cache/typed.rs` `#[cfg(test)]`) — starts a leader future in `remember_with_tags`, cancels/drops the leader mid-flight while a follower awaits `notified`, asserts the follower re-contends, executes `f` once to completion, populates the cache, and leaves `singleflight` map empty.
- `singleflight_leader_error_and_panic_clean_slot_without_stampede` (`crates/data/src/cache/typed.rs` `#[cfg(test)]`) — verifies both a leader returning `Err` and a panicking leader (`tokio::spawn` JoinError) drop their `SingleflightLeaderGuard`, wake waiting followers to re-contend cleanly (sequential retry rather than unguarded parallel `f()` calls), and leave `singleflight` map empty.
- `worker_fast_completing_job_never_leaks_abort_handle` (`crates/messaging/src/jobs/worker.rs` `#[cfg(test)]`) — dispatches a burst of instant no-op jobs and a hanging job through `JobWorker`, verifies completed tasks are reaped cleanly via `JoinSet` during steady state, and verifies `WorkerHandle::shutdown` timeout aborts remaining in-flight tasks without leaving orphan work.

## Verification

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -j 2 -- -D warnings
cargo test -p rume-data --all-features -j 2
cargo test -p rume-messaging --all-features -j 2
cargo test -p rume --test integration --all-features cache -j 2
cargo test -p rume --test integration --all-features concurrency_invariants -j 2
```

Expected: zero formatting/clippy warnings and all unit/integration tests pass.

## Commit

Only when the user asks:

```text
fix(concurrency): add RAII singleflight guard and replace manual worker handle tracking with JoinSet
```

## Done-when

- [ ] `Cache::singleflight` uses `std::sync::Mutex` and `SingleflightLeaderGuard::drop` handles removal + `notify_waiters()` with `Why:` comment.
- [ ] `remember_with_tags` loops follower re-contention when `get_json` returns `None`.
- [ ] `crates/messaging/src/jobs/worker.rs` replaces `in_flight_handles`, `next_job_seq`, and `InFlightHandleGuard` with `tokio::task::JoinSet` (`spawn`, `join_next()`, `abort_all()` / `JoinSet::drop`) and a `Why:` comment.
- [ ] All verification commands exit 0 with 0 warnings.
