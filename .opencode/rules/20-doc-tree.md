# Rule — Document tree

Where each kind of document lives. The *procedures* for producing them live in
`.opencode/skills/` — this file is only the map and the invariants that always hold.

| Tree | Holds | Procedure |
|---|---|---|
| `docs/plans/<NN>-<name>/` | One folder per phase: `README.md`, one file per task, `progress.md` ledger, committed `review/` if the plan was reviewed, git-ignored `scratch/` | `plan-orchestration` (producing one), `plan-authoring` (writing it), `plan-executor` (running it) |
| `docs/reports/reviews/v<NN>/` | Codebase review reports + `todo.md`, `progress.md`, git-ignored `scratch/` | `review-orchestration` (running one), `review-authoring` (one lens report), `workflow` (draining the todo) |
| `docs/reports/research/<topic>/` | External research, `<YYYY-MM-DD>-<slug>.md`: the question, the sources, the findings, the conclusion | — |
| `docs/specs/` *(optional)* | Design reasoning a project deliberately keeps as reference — never a rival of the code | — |
| `.opencode/rules/` | Always-loaded invariants (this tree) | — |
| `.opencode/skills/` | On-demand procedures, `<name>/SKILL.md` | — |

## Invariants

- **The code is the source of truth.** It defines current behaviour. There is no
  document that outranks it, and no spec/research step that must happen in a
  document before code changes. The one exception is an approved `docs/plans/`
  phase: once the user approves a plan, execution follows it per the
  `plan-orchestration` skill — but the plan still never outranks the code.
  Where plan and code disagree, the code is right and the plan gets a ruling.
- **Where a spec and the code disagree, the code is right.** Specs (when a
  project keeps them at all) hold *reasoning*, not authority: do not edit them
  or "bring them up to date" as part of another task.
- **`docs/plans/` is where forward-looking design lands.** A phase plan carries
  the decisions and constraints for the work it covers; it is written, reviewed,
  and executed per the `plan-orchestration` skill.
- **Reviews live where their subject lives.** A review of a *plan* goes to
  that plan's own `review/` folder (`docs/plans/<NN>-<name>/review/`),
  committed with it. A review of the *codebase* goes to the main review
  folder (`docs/reports/reviews/v<NN>/`) — never the other way around.
- **Review reports are non-authoritative.** They are mandatory deliverables of a
  review, but deleting one loses only a human-readable summary — the fixes
  themselves live in the code and its history.
