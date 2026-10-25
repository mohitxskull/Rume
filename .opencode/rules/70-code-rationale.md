# Rule — Record why, at the decision site

Where the code took one road and a reasonable reader would ask **"why not the
other one?"**, say so in a comment. Reviewers and auditors otherwise re-derive the
reasoning from scratch, or worse, file the deliberate choice as a defect.

## When

Only where the choice is not self-evident:

- the obvious approach was rejected for a reason the code cannot show (a race, a
  cost, an API that misbehaves);
- a constant is load-bearing (`19 * 1024` KiB, a 60s sweep interval) rather than
  arbitrary;
- the code looks roundabout, duplicated, or over-careful, and is deliberately so;
- a **ruling** made during execution changed the code
  (`.opencode/rules/50-agent-conduct.md`) — the ledger records the ruling, the code
  records why it looks like this.

Not everywhere. A comment on an obvious line is noise, and noise is what trains
readers to skip the comments that matter.

## Shape

Three parts, in whatever phrasing fits the file:

1. **the choice** — what this does instead of the expected thing;
2. **the alternative and why it lost** — concretely: what breaks, what it costs;
3. **revisit if** — the condition that would flip the decision, when there is one.

Start the sentence with `Why:` where it is not already a doc comment, so a
reviewer can grep every decision site in the tree.

Cross-cutting decisions go once in the module header rather than scattered.
Local ones sit on the line they explain.

## Not this

- **Narrating the code.** `// increment i` explains nothing a reader cannot see.
- **Change history.** "Used to be X, changed in the auth refactor" is git's job.
- **Restating a rule that has an owner.** Cite it instead.
- **A rationale for a decision you did not actually weigh.** Invented
  justification is worse than silence — it stops the next reader from thinking.

## Keep it true

When the decision changes, update or delete its comment **in the same edit**. A
stale rationale is worse than none: it actively misleads an auditor who has no way
to know it expired.

## For reviewers

A rationale comment is **evidence, not a verdict**. It tells you the choice was
deliberate, which is worth knowing — it does not tell you the choice was right.
Assess it like any other claim, and if you disagree, cite the comment in the
finding so the disagreement is about the stated reasoning rather than a guess at it
(`review-authoring` skill).
