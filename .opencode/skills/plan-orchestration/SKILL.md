---
name: plan-orchestration
description: Use when a project phase needs a plan produced or executed — dispatching a plan-writer subagent instead of writing the plan in the main session, running the question loop by resuming that same subagent with the human's answers, acting on the writer's own call about whether the plan needs review, then scaffolding the compiling skeleton and handing off to the plan-executor skill. Writing the plan file itself is the plan-authoring skill.
---

# Plan orchestration

This is the **conductor's** job across a plan's life: get the plan written by a
subagent, relay its questions to the human, then execute it. Writing the plan file
is a different job — see the `plan-authoring` skill, which is what the dispatched
writer follows.

**The main agent does not write the plan.** It dispatches a plan-writer subagent
and stays out of the way.

## Why a subagent writes the plan

Authoring a phase plan means surveying the current code, reading whatever design
material the project keeps, and often researching libraries on the web. That is
a lot of context for material that is **almost entirely irrelevant once the plan
exists** — the plan file is the distillate; the raw reading is not.

If the main session does that reading, it carries all of it into scaffolding,
dispatching implementers, and reviewing — the phases where clean context matters
most. Delegating keeps the main session holding the plan and the decisions, not
the search history that produced them.

The writer is also **free to browse the web** for the best available approach
(`.opencode/rules/50-agent-conduct.md`) — official docs, library references, current
API behaviour. Fact-checking there, not guessing project requirements.

## Checklist

1. Scope the phase with the human: what vertical slice, what is out of scope.
2. Dispatch the **plan-writer** subagent with a brief (§The brief).
3. Run the **question loop** until the writer has no open questions (§).
4. Receive the plan; verify it against `plan-authoring`'s finished-plan checklist.
5. The writer says whether the plan **needs review**. If yes, dispatch a plan
   reviewer, which writes into the plan's own `review/` folder
   (§Does the plan need review?).
6. Put the plan — and the review report, if there is one — to the **user for
   approval** before any execution begins.
7. Scaffold the **compiling skeleton** before executing anything.
8. Execute: one implementer subagent per task, one at a time, on `main`.

## The brief

The plan-writer gets, at minimum:

- the **phase goal** and what is explicitly out of scope;
- the **plan number and folder path** it must write (`docs/plans/<NN>-<name>/`);
- an instruction to follow the **`plan-authoring`** skill for format;
- pointers to the rules/design material most likely to bind, as a starting point —
  not a reading list to be treated as exhaustive;
- permission to **research on the web** and an instruction to cite what it used;
- an instruction to **return open questions rather than guess**, as a numbered
  list, when a decision is genuinely the human's;
- an instruction to end with a **review call** — does this plan need an
  independent review before execution, and why (§Does the plan need review?).

## The question loop

A plan-writer that guesses on a product call produces a plan that has to be
rewritten. So the loop is:

1. The writer returns either a finished plan or a numbered list of open questions.
2. The main agent puts those questions to the **human** — verbatim in substance,
   with enough context for each to be answerable, and its own recommendation where
   it has one.
3. The main agent **resumes the same subagent** with the answers.
4. Repeat until the writer returns a plan with no open questions.

**The writer returns questions; it never asks them directly.** Beyond keeping the
answers in the main transcript, this is a hard constraint in opencode: a pending
question is only rendered for the session in view and its direct children, and the
ask blocks with no timeout — so a question raised deeper in the tree hangs
silently (`.opencode/rules/50-agent-conduct.md`).

**Resume the same subagent — never dispatch a fresh one with the answers.** A new
agent starts cold: it re-reads the code, redoes the research, and can silently
reach different conclusions than the ones the questions were framed against. The
whole point of the loop is that the research context survives the round trip.

### How to resume

opencode reports each subagent's `session_id` — pass it back to resume that
same writer session with the answers.

## Verify the plan before acting on it

Run `plan-authoring`'s finished-plan checklist against what came back — required
sections present, no boilerplate or cross-task type signatures, constraints cited
rather than restated, task index matching the files on disk, every done-when box
checkable by a command.

A plan that fails the checklist goes back to the **same writer** (resume that
same session via its `session_id`) with the specific failures. Do not patch it yourself in the main session; that puts the
plan and its author out of sync for the next round.

There is no plan-review subagent — `docs/reports/reviews/` holds codebase reviews only
(`.opencode/rules/20-doc-tree.md`). Once the plan passes the checklist it goes to the
**user** for approval, and execution starts only after they give it.

## Does the plan need review?

**The plan-writer makes this call, not the conductor.** It has just spent the
whole job inside the problem: it knows which decisions were forced, which were
close, and where it had to choose between two defensible shapes. The conductor,
who deliberately avoided that context, is the worst-placed party to judge whether
the result is risky.

So the writer ends its return with an explicit verdict:

```
Review: yes — <what specifically should be checked, and why it worries me>
Review: no  — <why the plan is mechanical enough not to need one>
```

Say "yes" when any of these hold, and the writer should be told so in its brief:

- a decision could have gone another way and the plan picks one without the code
  forcing it;
- the phase touches something where a mistake is expensive to unwind — auth,
  data migrations, the wire contract, anything with a privacy surface;
- the task decomposition is novel rather than a repeat of a shape that has worked;
- the writer had to make a ruling on a question the human never answered.

Say "no" for a mechanical phase — a known shape, forced decisions, a small blast
radius. A plan that needs no review is a normal outcome, not a failure; a writer
that says "yes" every time has stopped making the judgement.

### Dispatching the plan reviewer

If the call is yes, dispatch **one fresh reviewer** (never the writer — see
`.opencode/rules/50-agent-conduct.md`) with:

- the plan folder path and the phase goal;
- **the writer's stated worry**, verbatim — that is the review's charter;
- the `plan-authoring` skill as the standard the plan is measured against;
- the output path: **`docs/plans/<NN>-<name>/review/plan-review.md`**.

Reviews of a plan live in that plan's own `review/` folder, committed with
it — never in `docs/reports/reviews/`, which holds codebase reviews only
(`.opencode/rules/20-doc-tree.md`). The report records *why the plan is
shaped this way*, which is exactly the kind of thing that cannot be
regenerated later.

A plan review asks what a compiler cannot: is the decomposition right, does each
task honour the constraints it cites, are the named tests the right tests, is
anything required missing. It does **not** hunt compile errors — a plan carries
tasks rather than code, and the skeleton is compiled before execution, so
undefined symbols and signature drift are the compiler's job.

Findings go back to the **same writer** (resume that same session via its
`session_id`), never patched in the main
session.

## Executing the plan

Execution is its own job with its own skill — **`plan-executor`**. Hand off to it
once the user has approved the plan. In outline, it makes you the controller:
scaffold the compiling skeleton, then one fresh implementer subagent per task file
(one at a time, on `main`), a task review after each, a 3-round fix loop, a ledger
at `docs/plans/<NN>-<name>/progress.md`, and a whole-plan review at the end.

**Scaffold the skeleton before handing off.** Project layout, type signatures,
migrations, and test files as named stubs with `todo!()` bodies; the project's
compile gate must pass and nothing works yet. Implementers read types from a
compiling project, never from a description of one
(`.opencode/rules/50-agent-conduct.md`).

## Working with superpowers `subagent-driven-development`

The execution loop above mirrors Superpowers' `subagent-driven-development`
(SDD) — but do not invoke it directly: it is denied in `opencode.json`, and
the adaptation below replaces it. Its task loop, its fix-loop escalation (rounds 1–3
resume the original implementer, 4–5 dispatch a fresh one on a more capable
model, breaker at 5) and its ledger are the design reference, not instructions;
where they conflict with what follows, what follows wins.

Five things differ in this repo. Where they conflict, **this repo's rules win** —
they are project decisions the user has made explicitly.

### Artifacts live in the plan folder, not a scratch workspace

SDD keeps every artifact in `.superpowers/sdd/<plan-basename>/` and **deletes it**
when the run finishes — taking every ruling the controller made with it. This repo
puts them in the plan's own folder instead: the ledger at `progress.md` and the
plan review at `review/`, both committed; review packages in `scratch/`,
git-ignored because a diff is already in git byte for byte.

Do not run `scripts/sdd-workspace` (its path is hardcoded, with no override) or
`scripts/task-brief` (it cuts one task out of a monolithic plan file — our task
files are already one per task). `scripts/review-package` **is** reusable: it takes
an explicit output path as its fourth argument. Details in `plan-executor`.

### No worktrees — work happens on `main`

SDD's setup step creates a git worktree. This repo does not: the user has
explicitly consented to working directly on `main`
(`.opencode/rules/30-git.md`). Skip the worktree step; skip
`superpowers:using-git-worktrees` entirely.

Note the consequence SDD relies on and you lose: a worktree makes
`git clean -fdx` recovery cheap and isolates a bad run. On `main` there is no
isolation, so the ledger and `git log` are your only recovery map — keep the
ledger current.

### Commits are not automatic

SDD's implementers commit per task, and its review packages are built from
`BASE..HEAD` commit ranges. This repo's rule is **no commits without the user
explicitly asking** (`.opencode/rules/30-git.md`), and each task file states its
commit message rather than executing it.

Reconcile it by asking the user once, at the start of execution, whether
implementers may commit per task for this run. If yes, SDD's loop works as
written. If no, review packages come from the working-tree diff instead, and
nothing is committed until they ask.

### There is no spec to argue from

SDD says "if the plan names a Spec, read that too: the spec is the authority the
plan argues from." Plans here do not name a spec — whether or not the project
keeps one (`.opencode/rules/20-doc-tree.md`), the authority is the task's
**`## Constraints honoured`** section and the code it cites.

### The final review is ours, and the fix loops are shorter

SDD ends by dispatching `requesting-code-review`'s single reviewer. Use the
`review-orchestration` skill instead — lens plan, staged agents, one report per
lens, and a `todo.md` fix order — landing in `docs/reports/reviews/v<NN>/`.

Fix loops are **3 rounds, not 5** — see `plan-executor` §The fix loop for the
round shape, which applies at both the task level and here. The final review gets
three batched fix waves rather than SDD's single one, each wave taking a whole
`todo.md` tier or solving group.
