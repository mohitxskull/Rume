---
name: review-authoring
description: Use when writing a single lens report for a code review — the canonical report structure, C/M/m severity definitions, the anatomy of a finding, the Prior decisions section on a re-review, positive notes, and the verdict. This is the reviewer's job; the conductor's job is the review-orchestration skill.
---

# Review authoring

You are **one lens** in a review. Your charter — what you look at and what you
ignore — comes from the review's `plan.md`. You produce **exactly one file**:
`<lens>.md`, written into the review folder named in that plan.

Do not write a consolidated report, do not summarise other lenses, and do not
edit the subject you are reviewing. A review reports; a fix pass fixes.

## Canonical structure — do not invent a new one

```
# <Lens> Review — vNN
## Summary        # 2–4 sentence overall verdict, from this lens's perspective
## Findings
### C<n> <title>  # critical findings, numbered per severity (C1, C2, ...)
### M<n> <title>  # major findings
### m<n> <title>  # minor findings
                 # each finding: file path, what the doc says, why it's a
                 # problem, suggested fix
## Positive notes # what is strong and should be preserved
## Verdict        # is the subject ready to proceed (e.g. for implementation plans)?
```

On a re-review, a **`## Prior decisions`** section is added before `## Findings`
(see below).

## Severity

| Level | Means | Test |
|---|---|---|
| **C** | Critical — wrong behavior or a security/privacy hole if implemented as written | Would shipping this hurt a user or leak data? |
| **M** | Major — a real design problem, or an inconsistency that forces an implementation-time decision | Would an implementer have to stop and ask? |
| **m** | Minor — polish, wording, consistency | Would fixing it change only how it reads? |

Number findings **per severity**, in order: `C1`, `C2`, `M1`, `M2`, `m1`. Qualified
by lens and review (`security v02 M1`) these are how a finding is referred to
anywhere else, so never renumber one after the report is written.

Severity is about consequence, not confidence. If you are unsure a defect is
real, say so **inside** the finding and grade it on what happens if it is —
don't quietly downgrade it to hide the uncertainty.

## Anatomy of a finding

Every finding carries four things, in this order:

1. **Where** — file path and `§`/line. A finding without a location cannot be
   acted on.
2. **What it says** — quote or tightly paraphrase the actual text/code. The
   reader must be able to check you without opening the file.
3. **Why it's a problem** — the concrete failure: the input that breaks, the two
   clauses that contradict, the invariant it violates.
4. **Suggested fix** — specific enough to act on. "Clarify this" is not a fix.

When the code carries a rationale comment for the thing you are about to flag,
read it first and **quote it in the finding**. It is evidence the choice was
deliberate, not proof it was correct (`.opencode/rules/70-code-rationale.md`) — but a
finding that argues against the stated reasoning is worth ten that argue against a
guess at it. A rationale that is *stale* — describing a decision the code no longer
makes — is itself a finding.

Two failure modes to avoid:

- **Unverified claims.** Read the file before you report on it. Do not infer
  what a function does from its name, or what a spec says from its title. Web
  access is allowed and encouraged for checking library/API behaviour against
  official docs.
- **Restating another lens's job.** If the plan gave security to another agent,
  a security defect you spot in passing is a one-line note, not a full finding.

## Your report feeds the TODO

After every lens report is in, the conductor derives a `todo.md` for the review —
the fix order, with findings from different lenses grouped by shared root cause
(`review-orchestration` skill §The TODO). Two things you write determine whether
that derivation works:

- **Stable ids.** Your `C1`/`M2`/`m1` labels are quoted in `todo.md` and in every
  later reference to the finding. They are permanent from the moment you publish.
- **A one-line-actionable finding.** The TODO compresses each finding to its
  location, its evidence, and its fix. If your finding cannot survive that
  compression, it was too vague to act on in the first place.

State the root cause explicitly when you suspect one — "this and the two logging
findings are the same missing rule" is exactly the signal the grouping needs, and
you are the only one positioned to see it inside your lens.

## Prior decisions (re-reviews only)

If the review has prior report(s), your report MUST contain a `## Prior
decisions` section that, for every finding the prior review(s) recorded **in your
lens's area**, states one of:

- **REAFFIRMED** — the prior decision still holds, with evidence;
- **CHALLENGED** — the prior decision is disputed, with cited reasoning, and a
  new finding if it is a defect; or
- **NOTE-NEW** — the prior review did not cover this area; your finding stands on
  its own.

Prior decisions are **inputs to be critically assessed, not conclusions to
accept.** You retain full independence to overturn an earlier call; the rule only
requires that overturning (or reaffirming) be *stated and justified*, so
disagreement is visible and reasoned. **Silent contradiction and silent
rubber-stamping are both failures of this rule.**

On a diff review you are also checking, for your area: (a) it did not regress,
(b) prior C/M/m findings are actually resolved, (c) the change introduces no new
issues.

## Positive notes

Required, and not filler. Name what is strong **and should be preserved** — a
later refactor that deletes a deliberate safeguard is the defect this section
exists to prevent. If you found nothing worth preserving, say that plainly rather
than padding.

## Verdict

Answer the question the mode asks:

- **whole-document** — "is this ready to implement?"
- **diff** — "is this change safe to accept, or must a fix pass land first?"

Give a clear answer. A verdict that avoids committing is not a verdict.

## Before you submit

- [ ] File is `<lens>.md` in the folder `plan.md` names — nothing written elsewhere.
- [ ] Every section of the canonical structure is present, in order.
- [ ] Every finding has location, quote, why, and a specific fix.
- [ ] Findings numbered per severity; none renumbered.
- [ ] `Prior decisions` present if prior reports exist, with a verdict per prior finding.
- [ ] Every claim was checked against the actual file, not inferred.
- [ ] Verdict answers the mode's question outright.
- [ ] Each finding is compressible to one actionable line for `todo.md`.
