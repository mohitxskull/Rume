# Progress ledger — audit-fix phase

## Convention

- Each task entry: `[ ] <task-id> <slug> — <status>` where status is one of `not started`, `in progress`, `done`, `blocked`.
- When a task is marked `done`, record the commit hash and the check command output summary in the same line.
- The controller (plan-executor) owns this file; implementers report via task files in `scratch/`.

## Tasks

- [ ] 01-rc02-worker-idempotency-lock-release — not started
- [ ] 02-rc04-outbox-claim-lease-fetch-failed-sql — not started
- [ ] 03-rc04-outbox-mark-failed-recursion-pool-acquire — not started
- [ ] 04-rc06-tenant-quota-sliding-window-ttl — not started
- [ ] 05-rc01-singleflight-lost-wakeup — not started
- [ ] 06-rc01-circuit-breaker-halfopen — not started
- [ ] 07-rc01-builder-shutdown-wakeup — not started
- [ ] 08-rc07-session-tenant-id-regenerate — not started
- [ ] 09-rc15-csrf-layer-secret — not started
- [ ] 10-rc14-safe-equal — not started
- [ ] 11-rc03-env-loading — not started
- [ ] 12-rc05-builder-and-error — not started
- [ ] 13-rc08-rate-limiter — not started
- [ ] 14-rc09-metrics — not started
- [ ] 15-rc10-saml — not started
- [ ] 16-rc11-macro-and-cli-db — not started
- [ ] 17-rc12-sql-check — not started
- [ ] 18-rc13-persistence-transaction — not started
- [ ] 19-rc16-saga — not started
- [ ] 20-rc17-lock-renew — not started
- [ ] 21-rc18-testing-server — not started
- [ ] 22-rc20-doctor-dag — not started
- [ ] 23-rc19-rc21-code-hygiene — not started

## Rulings

None yet.
