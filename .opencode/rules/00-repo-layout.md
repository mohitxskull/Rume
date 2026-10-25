# Rule — Repository layout

<!-- Adapt per project at import time. Delete what does not apply. -->

- **This repo is the source of truth.** All code, docs, plans, reports, and
  AI/dev configs live here.
- TODO: name any export/mirror/publish target and what it must **never**
  contain — typically:
  - `docs/` internal material (plans, reports, research)
  - `.opencode/` — every agent rule, skill and instruction file lives under it,
    so excluding that one directory excludes all of them
  - Secrets, `.env` files
