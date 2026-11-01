# AGENTS.md — <Project>

The map. Rules that always apply live in `.opencode/rules/` and are loaded with this
file; procedures live in `.opencode/skills/` and are loaded when you need them.

## Always-loaded rules (`.opencode/rules/`)

| File | Covers |
|---|---|
| [`00-repo-layout.md`](rules/00-repo-layout.md) | what is the source of truth; any export/mirror targets and what they must never contain |
| [`10-tech-stack.md`](rules/10-tech-stack.md) | the pinned stack and the decisions made against alternatives |
| [`20-doc-tree.md`](rules/20-doc-tree.md) | where plans, reviews and research live; the code is the source of truth |
| [`30-git.md`](rules/30-git.md) | no commits unasked, Conventional Commits, secrets, work on `main` |
| [`40-verification.md`](rules/40-verification.md) | run the whole check, never a grep for what you just changed |
| [`50-agent-conduct.md`](rules/50-agent-conduct.md) | never review your own work, one implementer at a time, TDD, handoff-by-file |
| [`55-receiving-feedback.md`](rules/55-receiving-feedback.md) | no performative agreement, verify before implementing, push back with reasoning |
| [`60-architecture.md`](rules/60-architecture.md) | Rume architecture constraints: explicit/typed/production-ready, refusals, config/env split, crate boundaries, pre-v1 |
| [`70-code-rationale.md`](rules/70-code-rationale.md) | record why at the decision site, so a reviewer doesn't re-derive it |
| [`80-quality-bar.md`](rules/80-quality-bar.md) | readability/maintainability/debuggability first, root causes not symptoms, fails-before proofs, zero-warning bar, honest reports |
| [`85-minimalism.md`](rules/85-minimalism.md) | lazy principal engineer: smallest thing that solves it, ladder, never-simplify list, defers to testing/quality-bar |
| [`testing.md`](rules/testing.md) | unit vs integration test placement, skip/ignore policy, fakes, how to write a good test |

## Skills (`.opencode/skills/`) — load the one that matches the task

| Doing this | Use this skill |
|---|---|
| **Thinking an idea through** — discuss, research, pressure-test, then hand off | `assistant` |
| **Given a review's `todo.md`** — work it to completion | `workflow` |
| **Given a plan folder** — execute it task by task | `plan-executor` |
| Needing a plan written for a phase | `plan-orchestration` |
| Writing or revising the plan file itself | `plan-authoring` |
| Running a code review | `review-orchestration` |
| Writing one lens report inside a review | `review-authoring` |

The first three rows are the everyday entry points: an idea goes to `assistant`,
a todo to `workflow`, a plan to `plan-executor`. The rest are reached from
those, or asked for by name. The pipeline is
`assistant → plan-orchestration → plan-executor`, with
`review-orchestration → workflow` closing the loop.

One skill sits outside this table because it isn't task-triggered:
`rust-conventions` (the PingCAP/TiKV style guide) auto-triggers on writing or
reviewing Rust code, independent of which pipeline stage you're in — an
implementer under `plan-executor`, a reviewer under `review-orchestration`, or
inline work outside the pipeline all pick it up the same way. Projects with no
Rust surface can ignore or remove it.

## Harness wiring

Opencode-only. Rules load via `.opencode/opencode.json` (`instructions`);
skills under `.opencode/skills/*/SKILL.md` and commands under
`.opencode/commands/*.md` are discovered automatically. The harness floor is
pinned in [`rules/10-tech-stack.md`](rules/10-tech-stack.md).

## Output

Default to concise output: short answers, no restatement of the obvious,
details only on request.

## Commands

| Command | Does |
|---|---|
| `/resume` | Resume an interrupted subagent, or continue current work |

## Quick verification reference

```
# TODO — one command per subject, from .opencode/rules/40-verification.md's table
```

Full rules in [`rules/40-verification.md`](rules/40-verification.md).
