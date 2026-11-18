# 19-rc16-saga.md

## Goal

Fix RC-16 saga coordinator defects in `crates/core/src/saga/coordinator.rs`:

1. Log-and-continue: `persist_state` logs persistence failures with `tracing::warn` and swallows them (~line 227-229), so a failed save silently leaves the saga record stale — subsequent resume sees wrong state. Fix: propagate `save_record` errors (or at least treat persistence failure as fatal to the step transition).
2. Missing post-step persist: after a step succeeds, `completed_names` is updated but state is not persisted until the *next* step's `Running` persist or the end. If the process crashes between steps, the completed step is lost and re-executed. Fix: persist `Running/completed` state immediately after each step succeeds (post-step persist).
3. Duplicate step names: `resume_forward` maps completed names to step indices via `.find(|s| s.name() == name)`, which returns the first match — duplicate step names break resume indexing. Fix: reject duplicate step names at `add_step`/`step` registration time.
4. Lost failure reason: `resume_compensation` replaces the original forward error with a generic `"resumed compensation encountered failure"` / `"resumed compensation completed successfully"`, losing `err`. Fix: preserve the original failure reason in the `Compensated`/`CompensationFailed` status on resume.

## Files touched

- `crates/core/src/saga/coordinator.rs` — `persist_state`, `run_with_id` (post-step persist), `add_step`/`step` (duplicate name check), `resume_compensation` (preserve original error).

## Constraints honoured

- Root cause, not symptom: persist failures surface; each completed step is durably recorded; duplicate names prevented at registration; original error preserved.
- No new dependencies, no `unsafe`.
- Record `Why:` comments for post-step persist and duplicate-name rejection.

## Done when (checkable by a command)

- `cargo test -p rume-core saga` passes.
- `cargo clippy -p rume-core --all-targets` and `cargo fmt --all --check` clean.
- Duplicate `add_step` names return an error / panic at registration.
- Post-step persist: after each successful step, `save_record` is called with that step marked completed.

## Verification (fails-before test idea)

Tests:
1. **Duplicate step names:** `saga.step("a", ...).step("a", ...)` → **before fix** accepted → test fails; **after fix** rejected → passes.
2. **Post-step persist:** run a 2-step saga with a recording `SagaStore`, kill/simulate crash after step 1 succeeds (before step 2); resume → step 1 is recorded as completed and not re-executed. **Before fix** step 1 re-executes (no post-step persist) → test fails.
3. **Preserved failure reason:** forward step fails, compensation runs, resume → `Compensated.error` / `CompensationFailed.error` is the original forward `err`, not a generic "resumed compensation" string. **Before fix** reason is lost → test fails.
