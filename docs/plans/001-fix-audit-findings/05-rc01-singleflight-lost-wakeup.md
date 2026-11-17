# 05-rc01-singleflight-lost-wakeup.md

## Goal

Fix RC-01 singleflight lost wakeup + tight loop in two places:

1. `crates/core/src/routing/health.rs` `HealthChecks::run()` (~lines 305-361): the `tokio::sync::watch`-based singleflight can lose a wakeup (leader clears `*sf = None` and sends on `tx`; a waiter that subscribes between the clear and the send, or after a notify that was already consumed, can hang in `rx.wait_for(...)`), and the `loop { ... continue; }` can busy-retry when the leader's report is not yet cacheable.
2. `crates/core/src/cache/typed.rs` `Cache::remember_with_tags` (~lines 167-210): the `tokio::sync::Notify` slot has a lost-wakeup race (leader calls `notify.notify_waiters()` before a waiter calls `n.notified()`, so the waiter's `notified()` future never resolves → hangs), and there is no cleanup on panic (if `f().await` panics, the singleflight slot is never removed and waiters hang forever).

Fix root cause:
- health.rs: restructure the singleflight so the leader's publish (cache write + watch send) cannot be missed by a follower, and the loop terminates (no busy-retry) — e.g. send on `tx` while the receiver is still registered, or use a `Notify` + flag with check-after-wait ordering, and remove the slot only after waiters are woken.
- cache/typed.rs: call `notify.notify_waiters()` while the slot is still present / before removal so a waiter that registered `notified()` observes it, and wrap the leader execution in a RAII drop guard that always removes the slot and notifies waiters (covers panic and error paths).

## Files touched

- `crates/core/src/routing/health.rs` — `HealthChecks::run()` singleflight loop (~lines 305-361).
- `crates/core/src/cache/typed.rs` — `remember_with_tags` singleflight slot management (~lines 167-210).

## Constraints honoured

- Root cause, not symptom: no `sleep()` / widened windows; fix the happens-before between leader publish and waiter wake.
- No new dependencies, no `unsafe`.
- Preserve existing semantics: exactly one execution for concurrent callers, followers observe `cached: true`.
- Record `Why:` comments at the publish/notify and guard sites.

## Done when (checkable by a command)

- `cargo test -p rume-core health` and `cargo test -p rume-core cache` pass.
- `cargo clippy -p rume-core --all-targets` and `cargo fmt --all --check` clean.
- `health_checks_singleflight_stampede_protection` still passes (10 concurrent calls → exactly 1 execution, 9 cached).
- No infinite loop / hang: a concurrent run with a slow check completes within a bounded timeout.

## Verification (fails-before test idea)

Add a regression test that hammers `run()` / `remember_with_tags()` concurrently with a deliberately slow check/closure:

1. **Before fix:** with staggered start (leader begins, followers join mid-flight), at least one follower hangs in `wait_for`/`notified()` beyond a short timeout, or the slot leaks and a subsequent caller blocks forever → test (with a `timeout` wrapper) fails.
2. **After fix:** all callers return within the timeout; exactly one execution occurred; followers see `cached: true`.

Use `tokio::time::timeout` around each concurrent call and assert none time out.
