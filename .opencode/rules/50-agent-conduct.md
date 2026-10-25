# Rule — Agent conduct

- **Never review your own work in-session.** Every review runs on subagents, one
  per lens, driven by a lens plan. See the `review-orchestration` skill.
- **One implementer subagent at a time.** Never dispatch multiple implementation
  subagents in parallel — they conflict.
- **Independent investigation may fan out.** The one-at-a-time rule binds
  implementers working a shared plan or todo — they conflict because they
  touch overlapping state. Unrelated, independently-diagnosable failures
  (e.g. three test files broken by different causes) may be dispatched to
  parallel investigation/fix agents in one batch, each scoped to one
  problem domain with no shared files. Merge results and run the whole
  check (`.opencode/rules/40-verification.md`) once all return.
- **Scaffold the compiling skeleton before executing a phase.** Implementers read
  types from a compiling project, never from a description of one. See the
  `plan-orchestration` skill.
- **TDD is the default** for implementation tasks.
- **Agents hand work over as files, never as pasted content.** Anything pasted
  into a dispatch — or printed back by a subagent — stays in the dispatcher's
  context for the rest of the session and is re-read on every later turn. So when
  one agent's output feeds another, tell it to write a markdown file into the
  run's `scratch/` and **return only the path plus a short summary**:

  ```
  <path to the file>
  <one line: what happened, and whether the output is usable>
  <at most a few lines the next agent needs that are NOT in the file>
  ```

  The summary routes; it never restates the file. The file must stand alone — its
  reader is a cold agent with no shared context. Give every file a name unique to
  its run and subject (`group-B-findings.md`, `review-a7f3..b2c1.diff`), so a
  stale file can never be read as a current one.

- **Run artifacts live in the run's own folder** — a plan folder for an execution,
  a review folder for a todo run. The ledger (`progress.md`) and any plan review
  are committed there; review packages, subagent reports and handoff files go in
  its git-ignored `scratch/`. There is no `.sdd/` or `.superpowers/sdd/`. See the
  `plan-executor` and `workflow` skills.
- **Subagents return questions; they do not ask them.** A dispatched agent that
  needs a human decision ends its turn with a numbered question list, and the agent
  that dispatched it relays them and resumes it with the answers. Do not have a subagent call an
  ask-the-user tool. Observed opencode behavior (v1.18.x): a pending question
  surfaces only for the session in view and its **direct** children — so a
  question from a depth-2 agent (an implementer under a dispatched executor)
  renders nowhere and stalls the run. Returning the numbered question list is
  the only form that works at every depth.

- **A running job rules rather than stalls.** Once the user has approved a plan or
  a run, conflicts, ambiguities and defects are yours to decide — record each as
  `Ruling: <what you decided> — <why> — <what it costs if wrong>` in the run's
  ledger and continue. A wrong ruling costs rework they can see and undo; a
  session parked on a question costs their whole day. **Four things stop you:** an
  irreversible or destructive operation; a security-sensitive action; a side
  effect outside this repository; a decision where every path forward is a guess.

- **Web access for reference is allowed.** Subagents and tools may browse the
  internet for reference material — official docs, library/API references,
  specifications, and external research. Use it for fact-checking and looking up
  current documentation; do not use it to fabricate or guess project-specific
  requirements.
