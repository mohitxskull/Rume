# 06-rc01-circuit-breaker-halfopen.md

## Goal

Fix RC-01 circuit breaker HalfOpen wedge in `crates/core/src/resilience/circuit_breaker.rs`.

`allow_request()` sets `half_open_in_flight = true` and allows exactly ONE trial request in HalfOpen state, but `on_success()`/`on_failure()` only reset `half_open_in_flight = false` unconditionally at the top. The wedge: if the single HalfOpen trial request's future is dropped/cancelled before completing (client disconnect, timeout, panic), `half_open_in_flight` stays `true` forever and no further HalfOpen trials are ever allowed — the circuit is wedged HalfOpen. Also `state()` and `allow_request()` duplicate the Open→HalfOpen transition logic.

Fix root cause:
- Track in-flight trial requests with a counter (or reset `half_open_in_flight` when a trial completes/fails/is dropped), so a dropped trial does not wedge the circuit. A trial that completes (success or failure) transitions appropriately; a trial that is abandoned must release the in-flight slot.
- Deduplicate the Open→HalfOpen transition into one helper used by both `state()` and `allow_request()`.

## Files touched

- `crates/core/src/resilience/circuit_breaker.rs` — `CircuitInner`, `allow_request`, `on_success`, `on_failure`, `state`.

## Constraints honoured

- Root cause, not symptom: no retry loop or widened window; the in-flight slot is released when a trial does not complete.
- No new dependencies, no `unsafe`.
- Keep `half_open_in_flight` semantics (one concurrent trial) but make it non-wedging.
- Record a `Why:` comment explaining the abandoned-trial release.

## Done when (checkable by a command)

- `cargo test -p rume-core resilience` passes (includes circuit breaker tests if present).
- `cargo clippy -p rume-core --all-targets` and `cargo fmt --all --check` clean.
- A test drops a HalfOpen trial future without completing it, then asserts a subsequent `allow_request()` still permits a new HalfOpen trial (not wedged).

## Verification (fails-before test idea)

Test:
1. Open the circuit (exceed failure threshold).
2. Advance past `reset_timeout` so it becomes HalfOpen.
3. Call `allow_request()` → true (trial #1), then **drop the returned future / do not call on_success or on_failure** (simulating client disconnect).
4. Call `allow_request()` again → **before fix** returns `false` (wedged, `half_open_in_flight` still true) → test fails; **after fix** returns `true` (slot released) → test passes.
