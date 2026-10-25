---
name: plan-executor
description: Use when executing an approved plan under docs/plans/ — you become the controller, dispatching one fresh implementer subagent per task file, reviewing each task, running the 3-round fix loop, keeping the progress ledger in the plan folder, and finishing with a whole-plan review. Writing the plan is plan-authoring; getting one produced and approved is plan-orchestration.
---

# Plan executor

**Entry points — all three are normal:** the user points you at a plan folder and
says run it; `plan-orchestration` hands off after the user approves a plan it just
had written; or `workflow` dispatches you for a solving group that earned a
full plan. Nothing upstream is required — an approved plan folder is enough.

You are the **controller**. You do not write code, do not fix findings, and do
not review work yourself. You dispatch, package, adjudicate, and record — which
is what keeps your context clean enough to coordinate a whole phase.

This is Superpowers' `subagent-driven-development` adapted to this repo's plan
format and rules. Where the two disagree, what is written here wins.

## Where things live

```
docs/plans/<NN>-<name>/
  README.md         # the plan — an input; never write to it
  01-<task>.md      # the task file IS the implementer's brief
  02-<task>.md
  progress.md       # the ledger — you own it, it is committed
  review/           # committed: the pre-execution plan review, if one was run
  scratch/          # git-ignored: review packages, task reports, handoff files
    .gitignore      # contains a single `*`
```

**Commit what cannot be regenerated; ignore what can.** Rulings and decisions are
irreproducible, so `progress.md` is committed and survives the run. A review
package is `git diff` output — already in the repository, byte for byte — so
`scratch/` is git-ignored and disposable.

There is no `.sdd/` and no `.superpowers/sdd/`. Do not run Superpowers'
`sdd-workspace` or `task-brief` scripts: the first points at a workspace this repo
does not use, and the second exists to cut one task out of a monolithic plan file
— **our task files are already one per task, so the task file is the brief.**

`review-package` *is* reusable, because it accepts an explicit output path:

```
<sp>/subagent-driven-development/scripts/review-package \
    docs/plans/<NN>-<name>/README.md BASE HEAD \
    docs/plans/<NN>-<name>/scratch/review-<base7>..<head7>.diff
```

Without bash, write `git log --oneline`, `git diff --stat` and `git diff -U10` for
the range into one file yourself. **Never dispatch a reviewer without a diff file.**

## The ledger

`progress.md`, append-only, first line `# Plan ledger — plan: docs/plans/<NN>-<name>/`.

It is your recovery map: after compaction, trust the ledger and `git log` over your
own recollection. Entry forms:

```
Task <N>: complete (<base7>..<head7>, review clean)
Task <N>: complete (<base7>..<head7>, <K> parked)
Task <N>: fix round <R>/3 (<X> addressed, <Y> open — <one-liners>)
Task <N>: minor (deferred): <one-liner>
Task <N>: parked — <finding> — Ruling: <why the code stands>
Ruling: <what you decided> — <why> — <what it costs if wrong>
```

On start, read it. Tasks with a `complete` line are **done — do not re-dispatch**;
resume at the first task without one. A task whose last line is a fix round is
mid-loop: resume at the next round.

## Setup

1. Read the plan's `README.md` once — goal, Global constraints, task index. Read
   `review/plan-review.md` if it exists.
2. Check `progress.md` and resume as above, or create it.
3. **Settle commits once, up front:** may implementers commit per task for this
   run? This repo's default is **no commits without the user explicitly asking**
   (`.opencode/rules/30-git.md`), but review packages are built from commit ranges.
   If whoever dispatched you already said commits are authorized — `workflow`
   does — take that as the answer and do not re-ask. If the answer is no, build
   packages from the working-tree diff instead. Either way, record it in the
   ledger. Local commits only; never push.
4. Work happens **directly on `main`** — no worktree, no branch
   (`.opencode/rules/30-git.md`). You therefore have no cheap `git clean` recovery:
   the ledger is the only recovery map. Keep it current.
5. Create a todo per task.

### Pre-flight scan

Before dispatching Task 1, scan the plan for conflicts and write the result to the
ledger **as a table** — one row per pair of tasks that share a file or an
interface (what one produces vs what the other consumes, and what you found), and
one row per task on whether its own text agrees with itself.

> "The scan is clean" without those rows is not a scan you ran.

Our format makes most of this readable rather than inferred: `Files touched` gives
the sharing pairs directly, and `Constraints honoured` gives what each task must
preserve. Rule on everything the scan surfaces before execution begins, and record
each ruling.

## Model selection

Use the least powerful model that can do each job, and **always state the model
explicitly** — an omitted model inherits your session's, usually the most
expensive, which defeats this entirely.

| Role | Model |
|---|---|
| Mechanical task (1–2 files, complete task file) | cheapest tier |
| Multi-file integration, judgment | standard |
| Design judgment, broad codebase understanding | most capable |
| Task reviewer | mid-tier floor, scaled to the diff's risk |
| Scoped re-review of a small fix diff | cheap-to-mid |
| Fix-loop round 3 | at least one tier above the implementer that got stuck |
| Whole-plan final review | most capable |

Turn count beats token price: the cheapest models often take 2–3× the turns on
multi-step work. Mid-tier is the floor for anything requiring judgment.

## The task loop

**Batch same-shape work.** Several tasks that are each the same small edit
repeated across files go to **one** subagent as one brief, reviewed as one diff.
Reserve one-dispatch-per-task for work needing its own judgment or tests.

**Hand artifacts over as files.** Anything you paste into a dispatch — or that a
subagent prints back — stays in your context for the rest of the session and is
re-read every turn. Paths, not contents.

### 1. Dispatch the implementer

Record BASE (`git rev-parse HEAD`) first — the review package needs it. Never
`HEAD~1`; it silently drops all but the last commit of a multi-commit task.

The dispatch contains exactly:

1. one line on where this task fits in the phase;
2. **the task file path**, introduced as "read this first — it is your
   requirements, with the exact values to use verbatim";
3. interfaces and decisions from earlier tasks the task file cannot know;
4. your resolution of any ambiguity you noticed in it;
5. the report file path (`scratch/task-<N>-report.md`) and the report contract.

Never paste prior-task history into a later dispatch. Never make a subagent read
the whole plan. The implementer **never dispatches subagents** — not helpers, and
never its own reviewer; review arrives from you, after the report.

Record the implementer's `session_id` in the ledger — fix rounds 1–2 resume
that same session via it.
**One implementer at a time** (`.opencode/rules/50-agent-conduct.md`).

### 2. Handle the report

| Status | Response |
|---|---|
| `DONE` | Build the review package, dispatch the task reviewer. |
| `DONE_WITH_CONCERNS` | Read the concerns first. Correctness/scope concerns get addressed before review; observations get noted and you proceed. |
| `NEEDS_CONTEXT` | Provide what was missing, re-dispatch. |
| `BLOCKED` | Diagnose: missing context → re-dispatch with it; needs more reasoning → more capable model; too large → split; plan is wrong → rule, ledger the ruling, re-dispatch carrying it. |

**Never** ignore an escalation or make the same model retry unchanged. If the
implementer says it is stuck, something has to change.

Where a task's implementation had a real fork — the obvious approach rejected, a
load-bearing constant, a deliberately roundabout shape — the implementer records
why at the decision site (`.opencode/rules/70-code-rationale.md`). When one of your
rulings changed the code, the ledger holds the `Ruling:` and the code holds the
comment; neither substitutes for the other.

A `DONE` report must carry the verification it ran — the whole check for the
subject, not a grep for what it just changed (`.opencode/rules/40-verification.md`).
A report without command output is not a `DONE`.

### 3. Review the task

Never skip it, and never accept a report missing either verdict — **spec
compliance and quality are both required**. The implementer's self-review does not
replace it.

The reviewer gets three paths — the task file, the report file, the review package
— plus the plan's Global constraints **copied verbatim** as its attention lens.

Do not pre-judge. If your reviewer prompt contains "do not flag", "don't treat X
as a defect", "at most Minor", or "the plan chose" — stop. You are pre-judging to
spare yourself a fix loop. Let the finding be raised and adjudicate it.

A reviewer may report **"cannot verify from diff"** items — requirements living in
unchanged code or spanning tasks. They do not block the review, but you resolve
each one yourself before completing the task; a confirmed gap enters the fix loop.

### 4. The fix loop — 3 rounds

Triggered by a spec ❌, any Critical or Important finding, or a "cannot verify"
item you confirmed as a real gap.

**Minor findings never enter the loop.** Ledger them
(`Task <N>: minor (deferred): …`) and point the final review at that list.

A finding that conflicts with what the task file mandates is **yours to rule on**:
weigh finding against plan text, decide, ledger the ruling before acting. Do not
dismiss a finding because the plan mandated it, and do not dispatch a fix that
contradicts the plan without a recorded ruling.

| Round | Who |
|---|---|
| **1–2** | **Resume the original implementer** — its context is intact; it knows the task, the code and its own choices. Send the open findings verbatim. |
| **3** | **Fresh implementer, one model tier up**, framed: "a prior implementer attempted this task N times; you own it now — read the report file for what was tried." A loop that survives two resumes usually means the implementer cannot see its own problem; fresh eyes and a capability bump in one move. |

Every round: the implementer fixes, re-runs the tests covering the amended code,
appends its fix report to the **same report file**, and returns the short contract.
Confirm the fix report carries the covering tests, the command, and the output
before re-dispatching the reviewer.

**The re-review is scoped** — package `FIX_BASE..HEAD` where FIX_BASE is the head
the previous review saw. The re-reviewer verdicts each finding ADDRESSED or NOT
ADDRESSED and flags new breakage in the fix diff only. New Critical/Important
breakage joins the open list; out-of-scope observations become deferred minors.

**Never fix findings yourself.** Controller fixes skip review and pollute the
context you need for coordination.

### The breaker

When round 3's re-review still leaves findings open, stop dispatching and
adjudicate each one — you hold the cross-task context the reviewer lacks:

- **Reviewer is wrong, or contestable** → park with a ruling saying why the code
  stands. The final review sees both sides.
- **Real, nothing downstream builds on it** → park with a ruling that says so.
- **Real and load-bearing** — a later task builds on it, or it reveals a plan
  defect → rule the smallest change that unblocks the dependent work, ledger it,
  and carry it into the next task's dispatch. Parking a structural failure
  silently lets every dependent task build on it.

**Adjudicate only at the cap.** Adjudicating earlier to end a loop is pre-judging
with a different name. Every adjudication is a ledger entry; a silent discard is
forbidden.

### 5. Complete the task

Append `Task <N>: complete (…)`, mark the todo done, move on. Never start the next
task while a Critical/Important finding is neither fixed nor parked-with-ruling.

## Whole-plan review

When every task is complete, run a full review of the phase via the
**`review-orchestration`** skill — lens plan, staged reviewers, one report per
lens, landing in `docs/reports/reviews/v<NN>/`. Dispatch on the most capable model.
Point the lens plan at the ledger's deferred-minor and parked lines so they get
triaged rather than silently dropped.

That review produces a **`todo.md`** — findings ordered into P0–P3 tiers with
solving groups. That ordering *is* your fix plan.

**Three fix rounds, each one batched dispatch:**

- A round is **one** fix subagent carrying a whole tier or solving group — never
  one fixer per finding. Per-finding fixers each rebuild context and re-run
  suites; that wave can cost more than every task combined.
- Round 1 takes P0 and P1. Later rounds work down the tiers.
- Each round ends with **one scoped re-review** of the fix diff.
- After round 3, adjudicate residuals exactly as the task-loop breaker does —
  park with rulings, or rule the load-bearing ones and ledger what you decided.

## Finish

Collect every ledger line containing `Ruling:` — pre-flight rulings, parked
findings, breaker adjudications, all of them — into your final message under
**"Rulings I made"**, in order, each with what it costs if wrong.

That list is the only place decisions you took on the user's behalf reach them.
`progress.md` is committed, so the rulings survive the session — but the user
still has to be told, not left to find them.

Then delete `scratch/` (regenerable scratch) and report completion.

## What stops you

Rule and continue — see `.opencode/rules/50-agent-conduct.md` for the four things
that stop a running job and the ruling format.
