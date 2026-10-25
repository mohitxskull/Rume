---
name: review-orchestration
description: Use when running or re-running a code review of this codebase — choosing whole-document vs diff mode, getting a lens plan authored, building the context package, dispatching staged reviewer subagents, placing/versioning the review folder, and deriving the todo.md fix order with its solving groups. Writing an individual lens report is the review-authoring skill.
---

# Review orchestration

This is the **conductor's** job: decide the shape of the review, produce the lens
plan, dispatch the agents, and put the output in the right folder. Writing a lens
report is a different job — see the `review-authoring` skill, which is what each
dispatched reviewer follows.

**Reviews always use subagents — one per lens. Never review your own work
in-session.**

**Every review lands in `docs/reports/reviews/v<NN>/`** — one folder per review,
a single flat sequence. Reviews here are of the **codebase**; design documents
are not their subject, and plan reviews never land here — they live in the
plan's own `review/` folder (`.opencode/rules/20-doc-tree.md`).

Review reports are mandatory deliverables but are **non-authoritative**: the fixes
themselves live in the code and its history, so deleting a report loses only a
human-readable summary.

## Checklist

1. Get the **mode** from the human (`whole-document` or `diff`) — the planner never
   decides mode.
2. If the human did not supply a lens plan, dispatch a **`lens-planner` subagent
   first**, before any reviewer. There is no silent default lens set.
3. Save the plan as `plan.md` in the review's folder **before execution begins**.
4. Build the context package per the plan's mode.
5. Execute the staged graph (≤ 3 stages, agents parallel within a stage).
6. Last stage's agents each write one `<lens>.md` report into the review folder,
   following the `review-authoring` skill.
7. Derive **`todo.md`** — the fix order — from the finished reports (§The TODO).

## Lens plan (drives every review)

Every review is driven by a **lens plan**. The plan is authored either by the
human (hand-written) or by a `lens-planner` subagent — dispatched *first*,
before any reviewer, when the human does not supply one. There is **no silent
default set**: if the human is silent, the planner subagent is always run.

The human decides the review **mode**; the planner does not. The planner reads the
review request + subject and decides:

- which lenses to run (name, scope/charter, output filename) — matched to the
  review's actual risk surface, not a fixed table;
- the staged execution graph (see §Staged orchestration below);
- on a re-review, which prior report files are **mandatory** context (see
  §Continuity).

The plan is saved as `plan.md` inside the review's folder (top-level `vNN/` for a
whole-document review; `<baseline>/diff/vNN/` for a diff review) before execution
begins.

## Review modes

The human chooses the mode per review. The mode determines the subject, the
context package, and the question the verdict must answer.

**Whole-document review** — the first review of a kind/subject, or an explicit
full re-review. Subject is the complete spec/plan/codebase at its current state,
with no baseline. Context package = full current files. Verdict asks "is this
ready to implement?"

**Diff review** — a repeat review of the change set made since a prior review,
its *baseline*. The review lands one level under the whole-document baseline as
`<baseline>/diff/v01` (then `v02`… for further diffs off the same baseline); the
true baseline it is checked against is recorded in `plan.md`. Subject is
uncommitted working-tree changes, a commit, or a branch comparison relative to
that baseline. The context package adds the baseline: the diff, before/after
versions of the changed files, and the **baseline's reports** as the regression
baseline — so every lens checks (a) its area did not regress, (b) prior C/M/m
findings are actually resolved, and (c) the change introduces no new issues.
Verdict asks "is this change safe to accept, or must a fix pass land first?" A
full re-review (a fresh whole-document review) is always available on explicit
request.

## Dispatching the reviewers

Each reviewer subagent gets, at minimum:

- its **lens name and charter**, verbatim from `plan.md`;
- the **context package** the mode calls for;
- the **output path** it must write — `<review-folder>/<lens>.md`;
- an instruction to follow the **`review-authoring`** skill for report structure,
  severity grading, and the verdict.

Lenses may browse the web for reference research/docs.

## Continuity between reviews

A re-review of a subject that already has prior review report(s) under
`docs/reports/reviews/` MUST carry that prior decision context forward.
This is what prevents a later audit from silently re-litigating or contradicting
an earlier one's reasoned conclusions.

- The lens plan (or the human, if supplying one) MUST list the prior review
  report file(s) as mandatory context for the reviewing agents — not as optional
  background.
- Each lens report must then carry a **Prior decisions** section
  (REAFFIRMED / CHALLENGED / NOTE-NEW per prior finding). That obligation lives
  with the reviewer — see the `review-authoring` skill.
- A re-review that deliberately starts from a blank slate (e.g. the prior reports
  were deleted) is permitted, but `plan.md` MUST state this explicitly rather than
  silently omitting prior context.

## Staged orchestration

A lens plan is a list of **≤ 3 sequential stages**. Each stage runs **1 or more
agents in parallel**; stages run **sequentially**. Agents in stage K receive the
report files produced by stages < K as context.

The **last stage's agents** write the deliverable lens reports into the review's
folder (top-level `vNN/` for a whole-document review; `<baseline>/diff/vNN/` for
a diff review). There is **no consolidated master report**.

## The TODO (execution order)

Once every lens report is written, the conductor derives one more file into the
same folder: **`todo.md`**, the order the findings should be fixed in.

Lens reports are organized by *lens* — the right axis for finding defects, the
wrong axis for fixing them. A defect's root cause is often shared across lenses
(four separate privacy/security findings turning out to be one missing
"no PII in logs" rule), and the cheapest fix order is rarely the severity order.
`todo.md` is that execution view.

**`todo.md` is not a master report.** It contains **no new findings, no verdicts,
no analysis** — only an ordering of findings that already exist in the lens
reports, each citing its source. The no-consolidated-master-report rule stands.
If deriving the TODO surfaces a defect no lens reported, that is a signal the lens
plan had a gap: say so to the human, and do not smuggle the finding in here.

Required for every whole-document review; for a diff review, whenever it produced
two or more actionable findings.

### Priority is not severity

Tiers are `P0`…`P3`, and they order by **what unblocks what**, not by C/M/m:

| Tier | Means |
|---|---|
| **P0** | Critical — fix first; a live exposure or a defect everything else is built on |
| **P1** | Major — fix before a named milestone (state which: "before any privacy-sensitive / production launch") |
| **P2** | Major robustness — fix before a later named milestone ("before multi-replica / internet-facing rollout") |
| **P3** | Minor hardening — schedule, non-blocking |

Each tier heading states its **fix-before condition**, so a reader knows what
shipping without it costs. A single finding may split across tiers when its halves
have different urgency — grade each half where it belongs and say which half is
which.

### Solving groups

Within a tier, findings that should be solved **together** are one lettered group
(`### Group A — <name> (solve together)`). Group findings when:

- they share a **root cause** — one fix, or one new spec clause, closes all of
  them (add a blockquote note under the group saying so);
- they live in the **same subsystem or files**, so one context switch fixes all;
- fixing one **changes the shape** of the others, and fixing them separately means
  doing the second one twice.

Order groups so earlier ones unblock later ones. A finding that shares nothing
stays ungrouped — do not manufacture groups for tidiness.

### Item format

```
- [ ] **<id> — <title>** (`<lens>.md` <id>).
      <file:line> and the concrete evidence. <the fix>.
```

- **`<id>`** is the lens report's local label (`C1`, `M2`, `m1`), qualified by lens
  where two lenses use the same label (`Privacy M1` vs `Security M1`). Never
  renumber — the id is how a finding is referred to everywhere else.
- Keep the evidence and the fix on one line each. The TODO is a work queue, not a
  re-explanation of the report.

Close with a **`## Counts`** section: per-tier totals and the C/M/m split. It is
the fastest check that no finding was dropped in the derivation.

### Header

Open the file with what it was derived from and the statement that it is ordered
by priority with items that must be solved together grouped.

## Versioning

Reviews live under `docs/reports/reviews/`. Two shapes, kept shallow (never nested
more than one level below the baseline):

- **Whole-document review** → top-level version folder, incremented:
  `v01/`, `v02/`, ...
- **Diff review** → always a child of its whole-document baseline, with a
  per-baseline counter starting at `v01`:
  `docs/reports/reviews/v<NN>/diff/v01/`, `.../diff/v02/`, ...
  The review it is *checked against* (its true baseline — the `vNN` review or a
  prior `vNN/diff/vMM`) is recorded in its `plan.md`; the folder itself is always
  one level under the whole-document `vNN`.

All lens reports from one review — plus its `plan.md` and `todo.md` — share that
review's folder.

Example: a diff review of changes since `v23` →
`docs/reports/reviews/v23/diff/v01/plan.md`,
`.../diff/v01/security.md`, `.../diff/v01/correctness.md`, `.../diff/v01/todo.md`
(lens filenames are decided by the plan; `plan.md` and `todo.md` are fixed).

## Baseline deletion

If a `docs/reports/reviews/v<NN>` folder is deleted:

- any dependent `<baseline>/diff/*` review is **void**;
- the next review of that subject is a **whole-document review** until a new `vNN`
  baseline exists.

