# Rume audit-fix phase — plan

**Phase:** `docs/plans/001-fix-audit-findings`  
**Goal:** Fix all 21 root-cause defects (RC-01 through RC-21) from the verified audit report. Fix root causes, not symptoms. Where a fix requires a rewrite, do the rewrite.

## Constraints (always honoured)

- **Single phase, one plan.** All 21 findings are in this plan, ordered CRITICAL → HIGH → MEDIUM → LOW. Do not re-litigate the ordering or the scope decisions.
- **Root causes, not symptoms.** Each task fixes the defect at its origin. Rewrites are expected where the existing code is fundamentally wrong (e.g. RC-04 mutual-default recursion, RC-06 quota leak).
- **No work outside scope.** Do NOT fix RC-02's `in_flight_handles` AbortHandle race (does not exist in current code — replaced by `InFlightGuard` counter). Do NOT reference `crates/xtask/` or `EnvError::MissingRequired` (do not exist). No `#[ignore]` to hide failures; relocate per `rules/testing.md` instead.
- **No new dependencies without a recorded `Why:` decision.** Every dependency addition needs a rationale comment at the decision site (`rules/70-code-rationale.md`).
- **No `unsafe` code** (the crate forbids it).
- **Commits:** implementers may commit per task with Conventional Commits (`fix:`/`feat:`/`refactor:`/`chore:`/`test:`). The user has authorized per-task commits. Commit messages must describe the actual change.
- **One implementer at a time** within this shared plan/todo (`rules/50-agent-conduct.md`). Independent investigation may fan out, but implementation of a shared plan is serial.
- **TDD is the default.** Each task's verification must include a fails-before test: a test that demonstrates the defect before the fix and passes after.

## Severity ordering

Tasks are numbered and sequenced so CRITICAL fixes land first, then HIGH, then MEDIUM, then LOW. Implement in file order within a severity band unless a dependency dictates otherwise (e.g. RC-04 SQL + repository changes are tightly coupled and should land together).

## Verification standard

The whole check runs as one command per subject (`rules/40-verification.md`):

```
cargo clippy --workspace --all-targets && cargo fmt --all --check && cargo test
```

- Run the **whole** check after any edit that moves or renames something; a targeted grep cannot see what a fix broke elsewhere.
- Zero warnings: clippy and fmt must be clean. Targeted `#[allow]` suppressions carry a same-line reason.
- Tests that touch the DB or HTTP surface use the shared harness (`tests/common`); never construct a raw pool or app state in a test body.
- Every task's "Done when" must be checkable by a command, and every task must include a fails-before verification idea.

## Deliverables

- `README.md` — this file (phase goal, constraints, severity ordering, verification standard).
- One task file per task (`NN-<slug>.md`), each with `## Goal`, `## Files touched`, `## Constraints honoured`, `## Done when`, `## Verification`.
- `progress.md` — empty ledger initially, updated by the plan-executor as tasks complete.
- `scratch/` (git-ignored) — run artifacts only, never solution code.

## Open questions

1. **RC-06 default window:** The scope fixes the TTL at `Duration::from_secs(60)` to match `retry-after: 60`, with a configurable `window: Duration` on `TenantQuotaLayer`. Confirm the default `window` is 60s and whether any caller relies on the current no-TTL behaviour.
2. **RC-17 `renew` signature:** Changing `DistributedLock::renew` to take `&mut LockGuard` (so the fencing token/TTL can be updated) breaks the public trait API. Confirm this is acceptable pre-v1, and audit all `DistributedLock` implementors for the signature change.
3. **RC-08 `RateLimiter::check` mechanism:** `block_in_place(|| Handle::current().block_on(...))` panics on a `current_thread` runtime. Decide the replacement: `spawn_blocking` with a locally-built `current_thread` runtime delegating to `check_async`, or make `check` Redis-unavailable and route all callers to `check_async`.
4. **RC-15 `CsrfLayer::new(secret)`:** Removing the no-arg `new()` is a breaking change. Confirm all existing callers (grep `CsrfLayer::new()`) and whether `Default` should be removed entirely or made fail-closed (secret-less).
5. **RC-13 `raw_sql` for empty params:** Confirm whether to add a new `raw_sql(no-params)` method or simply make the existing no-param `execute`/`query_scalar` Transaction paths use `map_sqlx_error` consistently, and what the exact signature should be.
6. **RC-10 `signature_references`:** Should an empty `signature_references` vector be rejected outright, or only in `parse_signed`/`with_verifier` mode where a signature is mandatory?

## Review verdict

Review: yes — several decisions (RC-08 replacement mechanism, RC-17 trait signature, RC-06 default window, RC-15 caller impact) could have gone another way without the code forcing a single path; these are recorded as open questions for the implementer to rule on.
