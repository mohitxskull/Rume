# 10-rc14-safe-equal.md

## Goal

Fix RC-14: `helpers/safe_equal.rs` `safe_equal` has a length-dependent loop.

The loop iterates `0..max_len` and reads `a_bytes.get(i).copied().unwrap_or(0)` / `b_bytes.get(i)`, folding `x ^ y` into `diff`. While it does not early-return on mismatch, the *number of iterations* depends on `max_len = max(a.len(), b.len())`, and the `unwrapped_or(0)` padding means comparisons of different lengths still run `max_len` iterations — but the observable timing can still leak length because the loop bound and memory access pattern scale with input length, and `black_box` only guards the accumulator, not the loop-bound leakage.

Fix root cause: make the comparison truly constant-time with respect to length — always iterate a fixed, length-independent bound (e.g. compare fixed 32-byte SHA-256 digests via `safe_equal_hashed`, or pad both inputs to a fixed block size before comparing). Prefer routing public secret comparisons through `safe_equal_hashed` (fixed 32-byte output) so timing is independent of input length.

## Files touched

- `crates/core/src/helpers/safe_equal.rs` — `safe_equal` (and document callers to use the hashed variant for secrets).

## Constraints honoured

- Root cause, not symptom: timing must not vary with input length.
- No new dependencies (sha2 already present), no `unsafe`.
- Preserve the existing API (`safe_equal` still works for equality), but make the security-sensitive path length-independent.
- Record a `Why:` comment explaining length-independent comparison.

## Done when (checkable by a command)

- `cargo test -p rume-core helpers` passes (safe_equal tests).
- `cargo clippy -p rume-core --all-targets` and `cargo fmt --all --check` clean.
- `safe_equal` no longer iterates a length-dependent bound for security-sensitive comparisons; secret comparisons use fixed-width digests.

## Verification (fails-before test idea)

A timing-sensitive assertion is hard to make deterministic in a unit test, so the fails-before test is structural:

1. Before the fix, `safe_equal("a", "aaaaaaaaaa")` iterates 10 times while `safe_equal("a", "a")` iterates 1 — a length-dependent loop bound exists (demonstrate by inspecting source / a unit test that the loop bound is `max_len`).
2. After the fix, security-sensitive comparison is performed on fixed-width (32-byte) hashes so the loop bound is constant regardless of input length (e.g. a test confirms `safe_equal_hashed` is used for token comparison, or `safe_equal` itself pads to a fixed width).

Since true constant-time cannot be reliably asserted in a portable unit test, the test asserts the *construction* (fixed-width digest, no `max_len`-bounded loop over raw input) rather than measuring nanoseconds.
