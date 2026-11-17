# 13-rc08-rate-limiter.md

## Goal

Fix RC-08: `RateLimiter::check` Redis backend (`crates/core/src/resilience/rate_limiter.rs` lines 216-219) uses `tokio::task::block_in_place(|| { tokio::runtime::Handle::current().block_on(r.check(key)) })`, which panics on a `current_thread` runtime (`block_on` cannot be invoked from within a runtime context). `RateLimitService::call` already uses `check_async`, but `check` itself is unsafe on `current_thread`.

Fix root cause: remove the `block_in_place` + `Handle::current().block_on` from `RateLimiter::check`'s Redis branch so it cannot panic on a `current_thread` runtime. Decide the replacement mechanism (open question 3): either delegate to `check_async` via a non-blocking spawn, or make `check` Redis-unavailable and route callers to `check_async`. Since `RateLimitService::call` is the only production caller and already uses `check_async`, the minimal fix is to remove the blocking branch from `check`.

## Files touched

- `crates/core/src/resilience/rate_limiter.rs` — `RateLimiter::check` Redis branch (~lines 211-221).

## Constraints honoured

- Root cause, not symptom: do not wrap in `catch_unwind` or retry; eliminate the blocking `block_on` inside an async context.
- No new dependencies, no `unsafe`.
- `RateLimitService::call` continues to use `check_async` (already correct).
- Record a `Why:` comment at the replaced branch.

## Done when (checkable by a command)

- `cargo test -p rume-core resilience` passes.
- `cargo clippy -p rume-core --all-targets` and `cargo fmt --all --check` clean.
- `RateLimiter::check` with a Redis backend does not panic when called on a `current_thread` runtime (test spawns a `current_thread` runtime and calls `check`).
- No `block_in_place` / `block_on` remains in `check`.

## Verification (fails-before test idea)

Test:
1. Build a `tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()` runtime.
2. Inside it, create a `RateLimiter` with a Redis backend and call `limiter.check("key")`.
3. **Before fix:** `block_on` panics ("cannot block on a runtime from within a runtime" / `current_thread` restrictions) → test catches panic or aborts → fails; **after fix:** `check` returns a result (or a clear async-only error) without panicking → passes.
