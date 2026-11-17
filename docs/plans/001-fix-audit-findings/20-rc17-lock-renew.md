# 20-rc17-lock-renew.md

## Goal

Fix RC-17: `DistributedLock::renew` takes `guard: &'a LockGuard` (immutable) in `crates/core/src/coordination/lock.rs` line 74, so a renewal cannot update the guard's fencing token or `expires_at`. A renewal that bumps the fencing token cannot write it back to the guard, breaking fencing semantics (stale fencing token reused after renewal → split-brain risk).

Fix root cause: change `renew` to take `&'a mut LockGuard` so implementations can update `fencing_token` and `expires_at` on renewal. Update all implementors and callers of `renew` accordingly.

## Files touched

- `crates/core/src/coordination/lock.rs` — `DistributedLock::renew` signature (line 74).
- Any implementors of `DistributedLock` and callers of `.renew(...)` found by grep.

## Constraints honoured

- Root cause, not symptom: the guard must reflect the renewed lease's fencing token/expiry.
- No new dependencies, no `unsafe`.
- This is a public trait API change (breaking) — acceptable pre-v1; record a `Why:` comment and note in open questions (see README Q2).
- Update all `renew` implementations to mutate the guard.

## Done when (checkable by a command)

- `cargo build` and `cargo test -p rume-core` pass.
- `cargo clippy --workspace --all-targets` and `cargo fmt --all --check` clean.
- `DistributedLock::renew` signature is `fn renew<'a>(&'a self, guard: &'a mut LockGuard, ttl: Duration)`.
- A renewal updates `guard.fencing_token` / `guard.expires_at` (observable via a test impl).

## Verification (fails-before test idea)

Test:
1. Implement a fake `DistributedLock` whose `renew` increments a fencing token and sets `expires_at`.
2. Acquire a guard, record `fencing_token`, call `renew(&mut guard, ttl)`.
3. **Before fix** `renew` takes `&LockGuard` so it cannot mutate `fencing_token` → the guard's token is unchanged after renewal → test fails; **after fix** `guard.fencing_token` is bumped and `expires_at` extended → passes.
