---
description: Resume an interrupted subagent, or continue current work
---

Resume any interrupted subagent by passing its `session_id` back with `continue`.
Never re-dispatch fresh when a resumable session exists — a fresh agent starts
cold and can silently reach different conclusions.

If no subagent was interrupted, continue the current work inline instead.
