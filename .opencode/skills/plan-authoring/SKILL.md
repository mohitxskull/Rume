---
name: plan-authoring
description: Use when writing or revising an implementation plan file — the docs/plans/<NN>-<name>/ folder layout, the README.md and task-file templates, what belongs in a task file and what does not, and scoping a phase as a vertical slice. This is the plan writer's job; getting a plan produced and executed is the plan-orchestration skill.
---

# Plan authoring

You are writing the plan itself. Every plan lives in **`docs/plans/<NN>-<name>/`** —
one folder per phase, never a single flat file — containing a `README.md` and
**one markdown file per task**.

This is the house format. It layers on top of general planning practice
rather than replacing it: use the `assistant` skill (or the human's own
judgment) to decide *what* to build, use this to decide *how the plan file is
written*. Do not invoke the Superpowers planning skills — they are denied in
`opencode.json`, and this repo's pipeline replaces them. Who writes the plan,
and what happens to it afterwards, is the
`plan-orchestration` skill.

## Folder layout

```
docs/plans/
  <NN>-<kebab-name>/          # NN is zero-padded, sequential, never reused
    README.md                 # the phase: goal, constraints, task index, done-when
    01-<kebab-name>.md        # one file per task, numbered in execution order
    02-<kebab-name>.md
    03-<kebab-name>.md
```

You write `README.md` and the task files, and nothing else. Execution adds
`progress.md` (the ledger), `review/` if a review was run, and a git-ignored
`scratch/` — all owned by the `plan-executor` skill. Never write to them, and never
put progress or status into `README.md`: it is an input handed to subagents, and
anything you add there lands in every dispatch's context.

- `<NN>` continues the global sequence (`docs/plans/15-mutation-testing/` → next
  plan is `16-`). Numbers are never reused, even if a plan is abandoned.
- Task numbers start at `01` and are in **execution order** — task 02 may depend
  on task 01's output, and the executing subagent reads them in sequence.
- A plan with genuinely one task still gets a `README.md` + `01-<name>.md`.
  The README carries the constraints; the task carries the work.

## A phase is a vertical slice

A phase produces something **runnable end to end**, not a horizontal layer.
Building the whole server before any client exists leaves the wire contract — the
one thing two implementations can disagree about — untested until the last phase.

## README.md template

`# Plan <NN> — <title>` then:

| Section | Required | Contents |
|---|---|---|
| `## Goal` | **yes** | What this phase closes and *why it matters*. Name the deliverables as a numbered list. If it closes a tracked item, cite it. |
| `## Current state` | when non-obvious | What already exists in the tree, so the implementer doesn't rebuild it. Mark it *"read before executing"*. |
| `## Global constraints` | **yes** | Rules that bind **every** task: what must not be touched, what runs on `main` via subagent, environment prerequisites, commit discipline. |
| `## Constraints honoured` | **yes** | What the phase must not break, cited concretely: the modules, functions and tests it has to keep working; the rules in `.opencode/rules/`; any prior review finding it must respect. **Cite, do not restate.** |
| `## Task index` | **yes** | One line per task file: `` `01-name.md` — one-sentence summary``. |
| `## Done-when checklist` | **yes** | `- [ ]` boxes, each independently verifiable by a command or an inspection. This is the phase's acceptance test. |
| `## Verification commands` | optional | The exact commands that prove the phase landed, when they differ from the per-task ones. |
| `## Flagged gaps` | optional | Spec/code ambiguities the implementer must resolve or explicitly document as out-of-scope. Surfacing these in the plan beats discovering them mid-task. |

## Task file template

A task is the smallest unit that carries its own test cycle and is worth a
fresh reviewer's gate — fold setup, config and docs into the task whose
deliverable needs them, and split only where a reviewer could meaningfully
reject one task while approving its neighbor.

`# Task <NN> — <title>` then:

| Section | Required | Contents |
|---|---|---|
| `## Goal` | **yes** | What must be true when this task is done. |
| `## Files touched` | **yes** | Explicit paths. Scope the blast radius up front — e.g. "test modules only". |
| `## Deliverable` | **yes** | The concrete artifact: the behaviour, the config file, the report. |
| `## Constraints honoured` | **yes** | The behaviour this task must preserve and the rules it must obey, cited by file/symbol/test. **This is the plan's real payload.** |
| `## Named tests (bodies omitted)` | **yes** for implementation tasks | Test names + what each must assert. See below. |
| `## Code-only decisions` | optional | The things only code can express — a migration's DDL, an exact constant, a fixed copy string. Kept short, commented with *why*; that *why* lands in the code as a rationale comment (`.opencode/rules/70-code-rationale.md`). |
| `## Verification` | **yes** | The literal command(s) and the expected output. |
| `## Commit` | **yes** | The Conventional Commits message, in a fenced block, headed *"only when the user asks"*. |
| `## Done-when` | **yes** | `- [ ]` boxes scoped to this task. |

Long procedural tasks may replace the middle with numbered `## §1`/`## Part 1`
sections, but `Goal`, `Verification`, `Commit` and `Done-when` stay.

## Plans hold tasks, not code

**A plan states what must be true. The compiler states whether the code is
consistent.** Markdown does not compile, so any code written into a plan can only
be checked by reading it, and a review loop with no type-checker on either side
has no fixed point: each fix pass introduces inconsistencies that only the next
read catches.

This is a rule learned the hard way: a first draft that carried its code inline
needed three diff-review rounds, each resolving the previous round's compile
blockers while adding new ones. Every one was the same error — a definition
changed without its dependents.

So, per task file:

| content | include? |
|---|---|
| Goal, files touched, deliverable | **yes** |
| The constraints the task must honour — behaviour to preserve, rules to obey — cited by file, symbol or test | **yes** — this is the plan's real payload |
| Named tests + what each must assert | **names and assertions yes, bodies no.** `unknown_and_wrong_password_are_indistinguishable_in_body_and_in_time` is a requirement; its mock-server setup is not |
| Verification command and commit | **yes** |
| A decision that only code can express — a migration's DDL, an exact constant (`19 * 1024` KiB), a fixed copy string | **yes**, kept short and commented with *why* |
| Boilerplate — interfaces, DI modules, imports, struct definitions, Gradle blocks | **no** |
| Cross-task type signatures (`Interfaces:` blocks) | **no** — see below |

## The skeleton carries the types, not the plan

Cross-task signatures are real information — task 7 must know what task 4
produced — but prose is the wrong place for them, because prose drifts silently.
Before executing a phase, scaffold a **compiling skeleton**: project layout, type
signatures, migrations, and test files as named stubs with `todo!()` bodies.
The project's compile gate must pass; nothing works yet.

An implementer subagent then reads the actual types instead of a description of
them, and every undefined symbol, changed signature or duplicate definition
surfaces in seconds rather than in a review round.

**Scale signal:** if a task file's code blocks are growing past a screen or two,
that content belongs in the skeleton.

## Open questions

If something genuinely cannot be decided from the code, its tests, or research —
a product call, a tradeoff only the human can rank — do **not** guess and do not
bury it in prose. Collect it as a numbered question list and hand it back to
whoever dispatched you (`plan-orchestration` skill §The question loop). You will
be resumed with the answers and your research context intact.

A question is worth asking only if different answers produce materially different
plans. Anything you can settle by reading the code or checking a library's docs is
not a question — go settle it.

## End with a review call

You decide whether the plan you just wrote needs an independent review before
execution — you are the only party who knows which decisions were forced and
which were close. End your return with one line:

```
Review: yes — <what should be checked, and why it worries you>
Review: no  — <why the plan is mechanical enough not to need one>
```

Say yes when a decision could have gone another way without the code forcing it,
when the phase touches something expensive to unwind (auth, migrations, the wire
contract, a privacy surface), when the decomposition is novel, or when you had to
rule on a question nobody answered. Say no for a known shape with a small blast
radius — that is a normal outcome, and a writer who always says yes has stopped
making the judgement. See `plan-orchestration` §Does the plan need review?

## Before you call a plan finished

- [ ] Every task file has `Goal`, `Files touched`, `Verification`, `Commit`, `Done-when`.
- [ ] No task file contains boilerplate or cross-task type signatures.
- [ ] Every constraint is **cited** — a file, symbol or test — not restated as prose.
- [ ] The README's task index matches the files actually on disk.
- [ ] Every done-when box is checkable by a command or an inspection, not by opinion.
- [ ] Any question you could not settle yourself is listed, not guessed at.
- [ ] The return ends with a `Review: yes/no` call and its reason.
