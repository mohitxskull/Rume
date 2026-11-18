# Plan review — `docs/plans/001-fix-audit-findings`

Single-lens review of the audit-fix phase plan against the `plan-authoring`
finished-plan checklist. Reviewed the README, `progress.md`, and all 23 task
files.

## Positive notes

- **Task index complete.** All 23 task files (`01`–`23`) are present on disk
  and match `progress.md` exactly (all `not started`). No missing or orphaned
  tasks.
- **Required sections present everywhere.** README has Goal, Constraints,
  Severity ordering, Verification standard, Deliverables, Open questions, and
  Review verdict. Every task file has Goal, Files touched, Constraints
  honoured, Done when, and Verification.
- **Every "Done when" is checkable by a command.** All tasks use `cargo
  test -p <crate>`, `cargo clippy`, `cargo fmt --all --check`, and targeted
  greps. No hand-wavy "verify by inspection" done criteria.
- **Every task has a fails-before verification idea.** All 23 Verification
  sections describe a before-fix failure and an after-fix pass.
- **Out-of-scope exclusions respected.** No RC-02 `in_flight_handles` work
  (task 01 is scoped to lock release only), no `crates/xtask/`, no
  `EnvError::MissingRequired`, no `unsafe`, no new dependencies without a
  recorded `Why:` (task 04 notes `uuid` is already present; task 17 declines
  `syn`; task 10 notes `sha2` is already present).
- **Constraints cited, not restated.** Tasks cite short constraint phrases
  (e.g. "Root cause, not symptom", "No new dependencies, no `unsafe`") rather
  than restating full rule text. No cross-task type-signature boilerplate.
- **Open questions are genuine design decisions.** All six (RC-08 mechanism,
  RC-17 trait signature, RC-06 default window, RC-15 caller impact, RC-13
  raw_sql, RC-10 empty `signature_references`) are API-breaking / caller-impact /
  mechanism choices that cannot be unilaterally derived from code. They are
  recorded in the README rather than assumed, honoring the writer's charter.
  Each corresponding task records its chosen decision with a `Why:` rationale
  (tasks 04, 09, 13, 15, 18, 20).
- **Severity banding: LOW last.** Task 23 (RC-19/RC-21 code hygiene) is the
  LOW band and is correctly placed last.
- **Severity ordering not re-litigated.** The README correctly instructs
  implementers not to re-litigate ordering or scope.

## Findings

All findings are minor (m); none block execution.

### m-1 — Severity bands not cited per task; CRITICAL→HIGH→MEDIUM→LOW not traceable

**File:** `README.md` and all task files.

**What's wrong:** The README asserts tasks are "sequenced so CRITICAL fixes
land first, then HIGH, then MEDIUM, then LOW" and says "Do not re-litigate the
ordering." But no task file is labeled with a severity band, and the audit
report's per-RC severity assignments are not cited anywhere in the plan. A
reviewer cannot confirm the ordering is sensible from the plan alone — for
example, RC-03 (secret values leaked into plaintext `values`, a
confidentiality defect) sits at task 11, mid-plan, which is hard to reconcile
with CRITICAL-first without labels.

**What to change:** Add a one-line severity band to each task file (e.g.
`Severity: CRITICAL — per audit report RC-03`) citing the audit report, or a
severity table in the README. Do **not** change the order — this is
traceability only.

### m-2 — RC-14 (task 10) fails-before test is structural/source-inspection

**File:** `10-rc14-safe-equal.md`.

**What's wrong:** The before-fix fails-before signal relies on "inspecting
source / a unit test that the loop bound is max_len." Inspecting source does
not execute against the code and cannot be run as a passes/fails signal via
`cargo test`, so there is no clean fails-before → pass cycle. The after-fix
side is executable (confirm fixed-width digest routing), but the fails-before
half is not.

**What to change:** Make the fails-before executable — e.g. a test asserting
that security-sensitive comparisons route through fixed-width digests / that
`safe_equal` no longer iterates raw input with a `max_len`-bounded loop over
raw input (observable behavior, not source text). Document explicitly that
nanosecond-precision timing cannot be unit-tested (a known limitation, not a
gap).

### m-3 — RC-15 (task 09) constructor fails-before is not a clean `cargo test` signal

**File:** `09-rc15-csrf-layer-secret.md`.

**What's wrong:** The fails-before idea is "the test file itself won't compile
before the fix" because it references the removed no-arg `CsrfLayer::new()`.
If that call remains in the test file, the file won't compile before **or**
after the fix, so `cargo test -p rume-core security` cannot establish a
fails-before → pass cycle.

**What to change:** Use a separate compile-fail fixture (e.g. `trybuild` or a
dedicated `tests/` file expected to fail compilation before the fix) so the
runnable test suite compiles both before and after, with the constructor
removal asserted by runnable tests plus a `Done when` compile check
(`CsrfLayer::new()` no longer compiles).

## Verdict

**Plan approved.**

The plan meets the `plan-authoring` finished-plan checklist: all sections
present, task index matches disk, every "Done when" is command-checkable,
every task has a fails-before idea, exclusions respected, open questions
genuine and recorded rather than assumed.

The three minor findings (m-1, m-2, m-3) are refinements to fold in during
execution (severity labeling, executable fails-before for RC-14, compile-fail
fixture for RC-15). None block execution or require re-planning, and no
resubmission is required. The writer should address them as each task is
implemented; record any rulings in `progress.md` per the ledger convention.
