# Rule — Quality bar

The standing directive: **be a perfectionist about what ships, minimalist
about what gets built** (see `85-minimalism.md`). Quality is the deliverable,
not a phase. Every artifact must be easy to **understand, maintain, and
debug** — in that spirit:

## Craft

- **Readability beats cleverness.** A junior engineer six months from now is the
  reviewer. Prefer the boring shape that explains itself; earn any cleverness
  with a why-comment at the decision site (see `70-code-rationale.md`).
- **Classify by principle, not convenience.** Where a thing lives follows from
  what it *is* (generic vs domain-specific, unit vs integration), never from
  which file was open when it was written. Junk drawers are a defect.
- **Small, honest diffs.** No drive-by refactors bundled into unrelated fixes;
  no scope swept into a commit its message doesn't describe.

## Correctness discipline

- **Root cause or nothing.** Symptoms are never papered over — no `sleep()`,
  retry loops, `#[ignore]`, or widened windows without a documented mechanism
  and ruled-out alternatives.
- **Falsifiable proof, every time.** Regression tests must demonstrably fail
  before the fix (fails-before), pass after. Verification is real pasted output,
  never a claim about output.
- **Zero-warning bar.** The project's lint and format gates (the project's
  clippy target — `make clippy` if it exists, else `cargo clippy
  --workspace --all-targets` — plus `cargo fmt --all --check` for Rust; the
  equivalent for any other stack) stay clean; a warning is debt with interest.
  Targeted suppressions carry a same-line reason.

## Honesty

- **Reports match disk.** A DONE claim whose cited evidence file doesn't exist
  (or whose counts don't reconcile) is treated as a process incident, not a typo.
- **Disagreement is duty.** Push back with reasoning when a instruction degrades
  quality; agree in writing once overruled, and record the ruling.

## Parallelism

- Work outside a shared plan/todo MAY run in parallel when file ownership is
  strictly disjoint, each agent stages explicit paths, and review still covers
  every diff. Inside a shared plan/todo, `50-agent-conduct.md`'s
  one-implementer-at-a-time rule governs. Parallelism never buys speed at the
  cost of review coverage.
