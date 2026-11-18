# 07-rc01-builder-shutdown-wakeup.md

## Goal

Fix RC-01 `AtomicBool`+`Notify` lost wakeup in `crates/core/src/app/builder.rs` `run_with_bound_listener` (~lines 1319-1339).

`wait_shutdown` does `while !f.load(Ordering::SeqCst) { n.notified().await; }`. The signal task does `sf_task.store(true, SeqCst); sn_task.notify_waiters();`. If `notify_waiters()` is called before `wait_shutdown` registers `n.notified()`, the notification is lost (tokio `Notify` does not latch), and `wait_shutdown` hangs forever waiting for a signal that already fired — blocking shutdown/drain.

Fix root cause: reorder so the flag is checked after registering for notification (check-after-wait), or make the notifier latch (e.g. switch to a `watch` channel or `notify_waiters` before `store` plus a re-check). The wait loop must observe a store that happened before the `notified()` registration.

## Files touched

- `crates/core/src/app/builder.rs` — `wait_shutdown` closure and the signal task (~lines 1320-1339).

## Constraints honoured

- Root cause, not symptom: no `sleep()` / widened window; fix the happens-before between `store(true)` + `notify_waiters()` and `notified()` registration.
- No new dependencies, no `unsafe`.
- Preserve graceful shutdown semantics (signal → set flag → notify → waiters exit).
- Record a `Why:` comment at the reordered wait/notify site.

## Done when (checkable by a command)

- `cargo test -p rume-core app::builder` passes.
- `cargo clippy -p rume-core --all-targets` and `cargo fmt --all --check` clean.
- `drain()`/`shutdown()` complete within timeout even when the signal arrives before the waiter registers.

## Verification (fails-before test idea)

Test the lost-wakeup race directly:

1. Create a `JobWorker`, start it, then call `drain(timeout)` while concurrently triggering the shutdown signal **before** the drain's internal wait loop registers on `shutdown_notify`.
2. **Before fix:** `drain` hangs beyond the timeout (lost wakeup) → test (wrapped in `timeout`) fails; **after fix:** `drain` returns `true` within the timeout.

Alternatively unit-test `wait_shutdown`-equivalent logic: store(true)+notify_waiters() called before notified() registration must still let the waiter return.
