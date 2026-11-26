# Task 02 — Split `crates/enterprise/src/saga/coordinator.rs`

## Goal

Split `crates/enterprise/src/saga/coordinator.rs` (2,411 lines, "Saga workflow coordinator, runner,
and persistence store") into `crates/enterprise/src/saga/coordinator/{mod,store,workflow,tests}.rs`
(tests moved verbatim) — behavior-preserving; `cargo build` + `cargo test -p rume-enterprise` green.

## Files touched

- `crates/enterprise/src/saga/coordinator.rs` — deleted; replaced by:
- `crates/enterprise/src/saga/coordinator/mod.rs` — module docs ("Saga workflow coordinator, runner,
  and persistence store."); `pub type BoxFuture<'a, T>` (used by `store` and `workflow`);
  `mod store; mod workflow;` + re-exports `pub use store::{SagaStatus, SagaRecord, SagaStore,
  InMemorySagaStore};` + `pub use workflow::{Saga, SagaReport, SagaRecoveryHandle, SagaRecoveryPhase,
  SagaRecoveryError, forward_idempotency_key, compensation_idempotency_key};` (so
  `saga/mod.rs:5-8` `pub use coordinator::{InMemorySagaStore, Saga, SagaRecord, SagaRecoveryError,
  SagaRecoveryHandle, SagaRecoveryPhase, SagaReport, SagaStatus, SagaStore, compensation_idempotency_key,
  forward_idempotency_key};` is unchanged); `#[cfg(test)] mod tests;` → `coordinator/tests.rs`.
- `crates/enterprise/src/saga/coordinator/store.rs` — `SagaStatus` (28-70, `is_success`,
  `is_terminal`), `SagaRecord` (74-90), `SagaStore` trait (93-141, `save_record_cas`, `load_record`,
  `list_incomplete`, `try_acquire_execution`, `release_execution`, `release_execution_sync`),
  `InMemorySagaStore` (145-188, `new`, `with_max_retained_terminal`, `prune_terminal`,
  `prune_terminal_locked` + `SagaStore` impl 190-278), `ExecutionClaimGuard` (282-314).
- `crates/enterprise/src/saga/coordinator/workflow.rs` — `Saga` + `impl<Ctx: Send + Sync + 'static>
  Saga<Ctx>` (453-1636): `new`, `with_store`, `with_in_memory_store`, `store`, `add_step`,
  `step`, `step_with_key`, `name`, `acquire_execution_claim`, `persist_state`, `run`,
  `run_recoverable`, `run_with_id`, `run_with_id_recoverable`, `resume`, `resume_recoverable`,
  `resume_from_handle`, `resolve_completed_indices`, `execute_forward_from_recoverable`,
  `compensate_completed_recoverable`; plus `SagaReport`, `SagaRecoveryHandle` (+Debug),
  `SagaRecoveryError` (+`new`, Debug/Display/Error/source), `SagaRecoveryPhase`,
  `forward_idempotency_key`, `compensation_idempotency_key`, `attach_saga_id`, `panic_message`,
  `current_timestamp`.
- `crates/enterprise/src/saga/coordinator/tests.rs` — the `#[cfg(test)] mod tests` moved verbatim
  (coordinator.rs:1645+).

## Deliverable

`saga::coordinator` still re-exports the same public surface; `Saga`, `SagaStore`, `InMemorySagaStore`,
`SagaStatus`, `SagaRecord`, `SagaReport`, `SagaRecoveryHandle`, `SagaRecoveryPhase`,
`SagaRecoveryError` and both key builders are reachable exactly as before
(`crate::saga::Saga`, etc.). No behavior changes (method bodies move as-is).

## Constraints honoured

- `crates/enterprise/src/saga/mod.rs:5-8` re-export list unchanged.
- Core umbrella tests (`crates/core/tests/integration/enterprise.rs` uses `rume::enterprise::*`)
  unchanged — the enterprise/saml re-export path is not touched by this task (Task 04).
- No behavior change; `cargo build` + `cargo test -p rume-enterprise` green.

## Named tests (bodies omitted — moved from `crates/enterprise/src/saga/coordinator.rs:1645+`)

- `run_includes_generated_saga_id_in_storage_error` — asserts `Saga::run` surfaces the generated saga_id in `SagaError::Storage`.
- `run_recoverable_preserves_mutated_context_across_storage_failure` — asserts the mutated `Ctx` is preserved via `SagaRecoveryHandle`.
- `forward_recovery_deduplicates_when_post_step_completion_write_fails` — asserts forward steps are deduplicated when the post-step completion write fails.
- `rollback_intent_preserved_when_first_compensating_write_fails` — asserts `Compensating` intent is persisted even when the first compensating write fails.
- `compensation_recovery_deduplicates_when_post_comp_write_fails` — asserts compensations are deduplicated when post-compensation writes fail.
- `compensate_crashed_rejects_success_and_preserves_compensated` — asserts compensation of an already-succeeded saga is rejected.
- `resume_rejects_mismatched_saga_name_and_unknown_step` — asserts mismatched saga names and unknown step names are rejected.
- `concurrent_resume_cas_allows_single_winner_and_retains_all_comp_errors` — asserts CAS gives a single winner under concurrency and retains all compensation errors.
- `overlapping_resume_is_blocked_by_exclusive_execution_claim` — asserts overlapping resume is blocked by the exclusive execution claim.
- `resume_from_handle_rejects_mismatched_saga_name_handle` — asserts mismatched handles are rejected.
- `dropping_pending_saga_future_outside_runtime_releases_in_memory_claim` — asserts dropping a pending saga future outside a Tokio runtime releases the in-memory claim.
- `resume_from_handle_rejects_compensating_already_succeeded_saga` — asserts a `Compensating` handle on an already-succeeded saga is rejected.
- `resume_from_handle_rejects_forward_handle_for_persisted_compensating_saga` — asserts a forward handle for a persisted-`Compensating` saga is rejected.

## Verification

```
cargo build
cargo test -p rume-enterprise
```

## Commit

Only when the user asks:

```
refactor(enterprise/saga): split coordinator.rs into coordinator/ store and workflow modules
```

## Done-when

- [ ] `crates/enterprise/src/saga/coordinator/` contains `mod.rs`, `store.rs`, `workflow.rs`, `tests.rs`;
  `crates/enterprise/src/saga/coordinator.rs` deleted.
- [ ] `saga::coordinator::{Saga, SagaStore, SagaStatus, SagaRecord, SagaReport, SagaRecoveryHandle,
  SagaRecoveryPhase, SagaRecoveryError, forward_idempotency_key, compensation_idempotency_key,
  InMemorySagaStore}` resolve (via mod.rs re-exports); `saga::Saga`, `saga::SagaStore` etc. resolve
  (via `saga/mod.rs`).
- [ ] `cargo build` and `cargo test -p rume-enterprise` green.
