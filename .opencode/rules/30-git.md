# Rule — Git and secrets

- **No commits without the user explicitly asking** — with one defined
  exception: a `workflow`/`plan-executor` run whose controller announced
  commit-per-group up front and the user did not object. That announcement is
  the authorization; record it in the run's ledger.
- Commit messages follow [Conventional Commits](https://www.conventionalcommits.org/en/v1.0.0/)
  (`feat:`, `fix:`, `docs:`, `refactor:`, `chore:`, `test:`, `build:`, ...).
- Secrets are never committed; config goes through env vars or an untracked
  local config file.
- **Implementation defaults to `main`.** No worktrees, no feature branches,
  unless the project overrides here. Confirm this default at import if
  isolation is ever required.
