# Plan ledger — plan: docs/plans/004-structural-cleanup/

Execution mode (settled by user): sequential — one implementer subagent per task, one at a time on `main`.
Commits: none — user approved execution but not commits; repo default (`rules/30-git.md`): no commits without the user explicitly asking. Ledger entries use the `no-commit` form.
Review agents: none (user: "we won't be using review agents, only implementer subagents") — controller-side adjudication per plan-executor step 3.
Models: not overridden — implementers inherit the session default (user asked for no specific models).

## Pre-flight scan (run before dispatch 1)

Base: `abf14328ec1b023f5b1334f01d5ebc0ad4b7c428`

### Task/pair conflicts (files or interfaces shared)

| Pair | Shared file(s) | What one produces vs what the other consumes | Finding |
|---|---|---|---|
| 05 → 06 | `crates/data/src/persistence/migration/runner.rs` | 05 creates `migration/{mod,preflight,runner,tests}.rs`; 06's constraint: callers in `migration/runner.rs` keep working unchanged (`crate::persistence::backend::X` paths). | Sequential: 05 then 06. |
| 06 → 09 | `crates/data/src/persistence/backend/mod.rs` | 06 creates `backend/{mod,validation}.rs` (bridge: `pub(crate) use validation::{...}` at `crate::persistence::backend::X`). 09's "Data-internal include path fixes (tree moved)" rewrite `sql/postgres/audit_insert.sql` → `../../../sql/postgres/audit_insert.sql` in `backend/mod.rs`. | 09 after 06. |
| 09 → 10 | `crates/data/sql` tree; `crates/messaging` trees | 10's gate: `find crates/data/sql -type d | awk -F/ '{print NF}' | sort -n | tail -1` ≤ 4; `find crates/messaging -name '*.sql'` empty; `grep -rn "persistence::sql::" crates` empty. | Sequential, 10 last. |
| 11 → 12 | `crates/foundation/src/util/html.rs` | 11 renames `helpers/` → `util/` (html.rs lands at `util/html.rs`); 12's Files touched assume `util/html.rs` exists. | 12 after 11. |
| 11 → 14 | — | Grep: no refs to `rume_foundation::helpers`/`rume::helpers` in `crates/testing/` (only a doc comment in `crates/testing/src/lib.rs:1`). Task 14 independent of 11. | No dependency — order anyway: 13, 14. |
| 13 | `./tmp/` (empty dir at rume root) | 13 deletes it; nothing references it. | Independent — any order. |

### Self-agreement spot checks (task files vs tree, spot-checked by conductor)

| Task | Check | Finding |
|---|---|---|
| 01 | `crates/messaging/src/jobs/worker.rs` = 3,286 lines; doc: "Background worker runner, handler registry, and graceful drain lifecycle" — agrees with split plan (registry/runner/heartbeat/drain). | Agrees. |
| 09 | `crates/data/src/persistence/sql{,.rs}` = 50 `.sql` files; README says 50; writer verified `find` counts. | Agrees (re-verify at dispatch: `find crates/data/src/persistence/sql -type f \| wc -l`). |
| 12 | `crates/foundation/src/helpers/html.rs` mixes `escape_html`/`strip_html_tags` (324) + `excerpt` (text) — writer read the file. | Agrees (verify at dispatch: `grep -n 'fn excerpt\|fn escape_html\|fn strip_html_tags' crates/foundation/src/helpers/html.rs`). |

## Rulings

| # | Ruling | Why |
|---|---|---|
| R1 | Commits on branch `rume-004-structural-cleanup` (created now at base `abf14328ec1b023f5b1334f01d5ebc0ad4b7c428`); all commits stay on the branch; **no push until the user gives permission** | User: "do all commits there, but don't push until I give permission". Repo default (no-commits-without-asking) overridden by explicit authorization for this branch. |
| R2 | No review agents | User constraint: "we won't be using review agents, only implementer subagents". Controller adjudicates. |
| R3 | Sequential execution | User: "Sequential (as written)". |
| R4 | No model overrides | User asked no specific models; harness forbids setting them unasked. |

## Tasks

- `Task 01: complete (abf14328..522201d, no review — user constraint)`
- `Task 01: minor (deferred): `execute_job` marked `pub(super)` + lease-heartbeat spawn extracted into `heartbeat.rs` (both in the plan's scope); rustfmt joined a few now-fitting signatures in `worker/tests.rs` — cosmetic, tests pass.`
- `Task 02: complete (522201d..9e368aa, no review — user constraint)`
- `Task 02: minor (deferred): clippy gate — cargo clippy --workspace --all-features -- -D warnings fails with clippy::too_many_arguments (8/7) in crates/messaging/src/jobs/worker/heartbeat.rs (rume-messaging, task-01 code; enterprise crate untouched by task 02). Ruling: parked — out of scope for plan 004; needs a follow-up fix (or targeted allow) before the workspace clippy gate is green.`
- `Task 03: complete (9e368aa..1424c0f, no review — user constraint)`
- `Task 03: minor (deferred): 6 intended `pub(crate)` visibility edits in `app/builder/mod.rs` (`run_inspect_mode`, `build_router_with_resolved`, `collect_and_validate_routes`, `merge_module_router`, `normalize_route_template`, `validate_middleware_config`) — cross-module calls after the split; behavior-preserving (report §Implementation notes).`
- `Task 04: complete (07c214d..6099528, no review — user constraint)`
- `Task 04: minor (deferred): public module path `rume::enterprise::saml` moved to `rume::saml` (module now at crate root; `src/enterprise/` now nests only `scim.rs` — the Phase 4 "double enterprise" note); no consumers of the old module path (grep `enterprise::saml` empty); type paths (`rume::enterprise::SamlParser`) still resolve — verified via `cargo test -p rume --features enterprise` green.`
- `Task 05: complete (6099528..a160a36, no review — user constraint)`
- `Task 05: minor (deferred): pre-existing warning noted by implementer — `sanitize_non_nested_block_comments` never used (`backend.rs:379`); verified pre-existing on the pristine tree via `git stash` + rerun; out of scope (task 06's bridge re-exports it — reconcile during task 06).`
- `Task 06: attempt failed (ses_e070a3a46ffet9FTQT7bEzw6MJ): implementer errored with "Free usage limit reached" (environmental, not code/plan). On-disk partial work: `backend.rs` deleted, `backend/{mod.rs (463), validation.rs (1025)}` untracked, no report. Ruling: re-dispatch a fresh implementer to inspect, finish, verify, and report; no rollback (files present, plausible sizes).`
- `Task 06: complete (a160a36..6c762a4, no review — user constraint; retry after environmental failure)`
- `Task 06: minor (deferred): 3 latent fixes by retry implementer — cfg-gated `PersistenceError` import, trait imports in moved tests, rustfmt bridge; 4 cfg-gated `pub(crate) use` statements instead of one grouped import (required by per-function cfgs); verified via `-D warnings` build, all-features clippy + tests 40/0.`
- `Task 05: minor (deferred): `cargo fmt --all --check` reports diffs in `crates/data/src/persistence/migration/preflight.rs` (long `include_str!` consts after the path rewrite; task-05 file) — task-06 implementer flagged a related fmt issue in `tests.rs`; actual diffs are in `preflight.rs`. Fix at end of plan.`
- `Task 07: complete (0b4552f..eed0115, no review — user constraint)`
- `Task 08: pending`
