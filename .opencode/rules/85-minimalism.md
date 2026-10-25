# Rule — Minimalism

Behave like a lazy principal engineer: **lazy about what gets built,
perfectionist about what ships** (see `80-quality-bar.md`). The best code is
the code never written — the smallest thing that solves the problem wins, but
only once the problem is understood.

## The ladder — stop at the first rung that holds

1. **Does it need to exist?** Speculative need is skipped, said so in one
   line (cf `55-receiving-feedback.md` — the same YAGNI-check, applied at
   construction time, not just to feedback).
2. **Already in this codebase?** A helper, util, type, or pattern that lives
   here is reused, not rewritten.
3. **`std` does it?** Use `std` (cf the `rust-conventions` skill — KISS,
   YAGNI, minimal public API).
4. **An installed dependency solves it?** Use it. No new dependency without
   a recorded decision (see `70-code-rationale.md`).
5. **Only then:** the minimum-scope solution that works — shortest diff that
   still respects proper organization, boring over clever. New files and
   folders are expected when classification demands it (`80-quality-bar.md`
   Craft): never crowd unrelated code into whichever file is open to shrink
   a diff. (Run artifacts — plan folders, review folders, ledgers, `scratch/`
   — are mandated by `20-doc-tree.md`, not solution scope, and are exempt.)

The ladder runs *after* understanding, never instead of it: read the code
the change touches and trace the real flow first. The smallest change in the
wrong place is a second bug, not laziness.

## Binds scope, never depth

This rule governs the *proposed scope*, never the *depth of decision-making* —
including the `assistant` skill's pressure-testing. Do the full trade-off
analysis on decisions and architecture and record it; name the lazier
alternative and what would have to be true to need more, without defaulting
to it. A thin decision is not minimalism.

## Never simplify away

- Input validation at trust boundaries.
- Error paths that prevent data loss (`Result` handling stays).
- Security measures.
- Anything explicitly requested.
- Decision depth: architecture reasoning, trade-off analysis, and `Why:`
  records stay complete.
- File and folder organization: readability and proper placement stay; never
  merge unrelated code to save files.
- No `unsafe` to save lines, and no `#[ignore]`/skip to shrink a suite to
  fit the aesthetic.

## Defers to — this rule never overrides

- **Tests are not bloat.** Placement follows `testing.md`, proof follows
  `80-quality-bar.md` (fails-before), TDD stays the default
  (`50-agent-conduct.md`).
- **The plan skeleton is exempt.** The compiling `todo!()` skeleton that
  `plan-authoring`/`plan-executor` require is process scaffolding, not
  solution scope.
- **Deliberate simplifications are recorded** as `Why:` comments at the
  decision site (`70-code-rationale.md`), not smuggled in silently.
