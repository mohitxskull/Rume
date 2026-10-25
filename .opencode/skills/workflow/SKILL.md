---
name: workflow
description: Use when working a review's todo.md end to end — draining its solving groups one at a time via subagents (a scout re-checks each group, an executor runs it, the controller only records outcomes), then a diff review against the baseline, repeating until a review comes back with nothing actionable.
---

# Working a todo to completion

**Entry point:** the user hands you a `todo.md` path — that is the whole trigger.
Everything below follows from it; you do not need a plan, a phase, or a brief.
(If they hand you a *plan* instead, that is the `plan-executor` skill, used
directly — this skill is not in the path.)

A code review produces `docs/reports/reviews/v<NN>/todo.md`: findings ordered into
P0–P3 tiers with **solving groups** — sets of findings that must be fixed together,
ordered so earlier groups unblock later ones (`review-orchestration` §The TODO).

That is already a dependency-ordered work queue. This skill drains it.

You are the **controller**. You do not read findings, fix them, or review work.
You dispatch, you record, you decide. You also never *add* findings — this
workflow consumes a review's output, it never authors it. A defect you notice that
no lens reported goes to the user and into the next review's scope.

## Context discipline — the constraint everything else follows from

This loop runs longer than anything else in the repo: a dozen groups, each with a
plan, tasks, reviews and fix rounds. **If the controller reads any of that, the run
dies of context exhaustion halfway through.** Everything you paste in or a subagent
prints back stays resident for the rest of the session and is re-read every turn.

So the controller's context holds exactly three things:

1. the **queue** — open group ids and one-line titles;
2. the **ledger lines** it writes;
3. each dispatch's **short return**.

Nothing else. Concretely:

| Work | Who does it | What reaches you |
|---|---|---|
| Read `todo.md`, list open groups | scout subagent | group ids + one-liners |
| Read a group's findings, re-check them | scout subagent | one verdict line per finding |
| Read the lens reports | nobody — they are passed as **paths** | — |
| Write the plan | plan-writer subagent (`plan-orchestration`) | plan path, questions, review call |
| Run the plan | executor subagent (`plan-executor`) | status + ledger path |
| Fix a direct-route group | implementer + reviewer subagents | status + finding verdicts |
| Diff review at the end | review conductor subagent (`review-orchestration`) | new todo path + verdict |

**Hand artifacts over as file paths, never as contents**
(`.opencode/rules/50-agent-conduct.md`). A group's findings go into a file the next
agent reads; they never pass through you. Every dispatch names the file it must
write into `scratch/` and states its return contract — path, one status line, and
only what is *not* in the file. An agent that returns its working is an agent that
filled your context.

If your harness cannot nest subagents deeply enough for an executor to dispatch its
own implementers, fall back to running `plan-executor` inline — and compact between
groups, because that is exactly the cost this structure exists to avoid.

## Two loops

```
outer: does todo.md have open groups?
  inner: pick the next open group, in the todo's own order
         dispatch scout      → findings extracted to a file + re-check verdicts
         weight test         → plan route | direct route
         dispatch executor   → the group is fixed and reviewed
         tick the boxes, append to the ledger
         → next group
  todo drained
  → dispatch a diff review against the review that produced it (v01 → v01/diff/v01)
  → its todo.md becomes the new queue → outer loop again
stop when a diff review returns nothing actionable
```

Termination is a **clean review, not a counter.** The inner loop drains one
review's findings; the outer loop proves the fixes broke nothing and catches what
the first pass missed.

## Start of run

1. Dispatch a **scout** to read `todo.md` and return the open groups (`- [ ]`) as
   ids plus one-line titles, in the todo's own order. A partially drained todo is
   resumed, not restarted. This is the only view of the todo you hold.
2. **Ask the user once, for the whole run:** approve executing every group, or
   approve group by group? Default to once-for-the-run. Either way you still stop
   when a plan-writer returns **open questions** (`plan-orchestration` §The
   question loop) — that is where judgement is genuinely theirs, and it is the only
   routine interruption.
3. Create the run ledger at `docs/reports/reviews/v<NN>/progress.md`, first line
   `# Todo ledger — todo: <path>`. Committed; `scratch/` alongside it is git-ignored
   scratch, same convention as a plan folder (`plan-executor` §Where things live).
4. One todo per run. Never interleave groups from two reviews.

## Per group

### 1. Scout it

Dispatch one subagent with the todo path and the group id. It:

- writes that group's findings **verbatim** to
  `docs/reports/reviews/v<NN>/scratch/group-<X>-findings.md`;
- re-checks each finding against the current tree — the todo was written against a
  snapshot, and earlier groups have changed it since;
- returns **one line per finding**: `<id>: holds | already-fixed by <what> | no
  longer applies — <why>`, plus a one-line note on whether the work needs a
  decomposition (more than one task in a required order).

Cheap reads, not a review. It does not fix anything and does not judge severity.

**Never silently skip a finding.** `already-fixed` and `no longer applies` are
ruling-grade outcomes: tick the box with the reason and ledger the ruling. A box
ticked without a reason is indistinguishable from work that was done.

### 2. Route it

| The group is | Route |
|---|---|
| Several entangled findings; needs decomposition, new tests, a migration, or a design decision | **Plan route** |
| One or two mechanical findings, one subsystem, fix obvious from the finding | **Direct route** |

The scout's decomposition note decides borderline cases: more than one task in a
required order → plan route; one edit plus its test → direct route.

A full plan is heavy — plan-writer, question loop, a folder of task files, a
compiling skeleton, per-task reviews. Right for entangled work, absurd for a
one-line fix.

### 3. Execute it

**Plan route.** Hand off to `plan-orchestration`, giving the plan-writer the
**findings file path** (not the findings), the lens report paths as reasoning, and
the source to cite in the plan's Goal: `reviews/v<NN>/todo.md` Group `<X>`. The
plan lands at `docs/plans/<NN>-<name>/`, `<NN>` continuing the global sequence.

Then dispatch a **fresh executor subagent** to run `plan-executor` on that plan.
It writes its own ledger into the plan folder and returns status plus that path —
task detail, fix rounds and review findings never reach you.

**Direct route.** Dispatch one fresh implementer with the findings file path and
the files it names, then **one task review** of the diff — same rules as
`plan-executor` §Review the task, including the 3-round fix loop and the ban on
pre-judging the reviewer. Review packages go to `scratch/`.

The task review is not optional here. "It was a small fix" is precisely the
reasoning that lets a small fix ship broken.

### 4. Record and commit it

**A group lands as a commit.** Running this skill *is* the standing authorization
the git rule requires (`.opencode/rules/30-git.md`) — say so in your opening message
so the user can wave you off, then commit as you go rather than leaving a run's
worth of work uncommitted. Tell the executor subagents you dispatch that commits
are authorized for this run, so they do not stop to ask.

**Local commits only. Never push.**

Commit when the group's review is clean, or its residuals are parked with rulings.
Never commit a group that is mid-fix-loop: a commit that does not pass the whole
check (`.opencode/rules/40-verification.md`) is a broken bisect point, and the
next person to land on it pays for it.

**The work, the ticks and the ledger line go in one commit.** If the code lands but
the tick does not, a resumed run re-does work it already did — and the tick is the
only state this workflow has. Stage the source changes, `todo.md`, and
`progress.md` together.

Message follows Conventional Commits, and its body names the findings it closes so
history stays traceable back to the review that found them:

```
fix(auth): return generic UNAUTHENTICATED when the per-account limit trips

An IP-rotating attacker could confirm which emails exist by watching for 429.
The server-side delay still throttles brute force.

Closes: security v01 C1 — reviews/v01/todo.md P0
```

`Closes: <lens> <review> <sev><n>` is the code-side trace from a commit back to
the finding and the report that raised it. Multiple findings in one group get
multiple `Closes:` lines.

Then tick the group's boxes in `todo.md` and append one line to the ledger:

```
Group <X>: complete via plan <NN>-<name> (<K> findings, <commit7>)
Group <X>: complete direct (<K> findings, <commit7>)
Group <X>: <finding id> — no longer applies — Ruling: <why>
Group <X>: parked — <finding id> — Ruling: <why it stands>
Ruling: <what you decided> — <why> — <what it costs if wrong>
```

`todo.md` carries the ticks; `progress.md` carries the reasons; git carries the
change. All three committed, so a reader six months out sees what was done, why,
and exactly what it touched — without a session transcript.

A group that ends with everything ticked as `no longer applies` has no code change
and so no commit: record it in the ledger and commit the bookkeeping alone.

## When the todo is drained

Dispatch a **diff review** via `review-orchestration`, mode `diff`, baseline = the
review that produced this todo. It lands at `v<NN>/diff/v01`; its context package
includes the baseline's reports, so every lens checks that its area did not
regress, that prior findings really are resolved, and that the fixes introduced
nothing new.

Point its lens plan at this run's **ledger** — the parked findings and the
no-longer-applies rulings especially. A reviewer that cannot see a ruling will
re-raise the finding it settled.

It returns a new `todo.md` path and a verdict. Actionable groups → that becomes the
queue, outer loop again. Nothing actionable → the run is done.

## Finish

Report to the user:

- groups completed, and by which route;
- plans created, by number and name;
- **"Rulings I made"** — every `Ruling:` line from the ledger, in order, each with
  what it costs if wrong. This is the only place decisions taken on their behalf
  reach them;
- anything parked, and what it is waiting on;
- the final review's verdict.

Then delete `scratch/` — regenerable scratch.

## What stops you

Rule and continue — see `.opencode/rules/50-agent-conduct.md` for the four things
that stop a running job and the ruling format. A plan-writer's open questions also
stop you; those go to the user, and the run resumes on their answer.
