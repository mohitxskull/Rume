# Plan ledger — plan: docs/plans/001-fix-audit-findings/

Commits: authorized per-task (Conventional Commits, local only, per `README.md`).
Worktree/branch: `main` (direct, per `rules/30-git.md`).

## Pre-flight scan (resumed at Task 16)

Ruling: Tasks 01–15, 17, and 22 were already committed on `main` (`fce5006..f98b6c6`) prior to this `plan-executor` run, while `progress.md` was still unpopulated and Task 16 had uncommitted working-tree edits. Rather than re-implementing already-committed tasks (which would duplicate or overwrite work), Tasks 01–15, 17, and 22 are marked complete with their commit hashes and deferred to the whole-plan review (`fce5006..HEAD`). Tasks 16, 18, 19, 20, 21, and 23 will execute through the full `plan-executor` implementer + reviewer loop. — Costs rework only if any prior commit missed a requirement, which the whole-plan review will catch.

### Remaining-task file & interface overlap

| Pair / Task | Shared file or interface | Finding |
|---|---|---|
| Task 18 & Task 23 | `crates/core/src/persistence/mysql.rs` | Task 18 updates `Transaction` error handling (`map_sqlx_error`); Task 23 aligns inline SQL comments afterward. No conflict if serialized (18 before 23). |
| Task 16 | Self-check (`16-rc11-macro-and-cli-db.md`) | Consistent: `crates/macro/src/lib.rs` + `crates/cli/src/commands/db/mod.rs`. Uncommitted WIP exists in working tree; implementer will verify, complete, test, and commit. |
| Task 18 | Self-check (`18-rc13-persistence-transaction.md`) | Consistent: `postgres.rs`, `mysql.rs`, `sqlite.rs` `Transaction` error mapping + raw/no-param path. |
| Task 19 | Self-check (`19-rc16-saga.md`) | Consistent: `crates/core/src/saga/coordinator.rs`. |
| Task 20 | Self-check (`20-rc17-lock-renew.md`) | Consistent: `crates/core/src/coordination/lock.rs` + all `DistributedLock` impls/callers (open question Q2: breaking pre-v1 trait change accepted). |
| Task 21 | Self-check (`21-rc18-testing-server.md`) | Consistent: `crates/core/src/testing/server.rs`. |
| Task 23 | Self-check (`23-rc19-rc21-code-hygiene.md`) | Consistent: dead code (RC-19) & comment alignment (RC-21), excluding non-existent `crates/xtask/` and `EnvError::MissingRequired`. |

## Tasks

- Task 01 (`01-rc02-worker-idempotency-lock-release`): complete (`fce5006..5c1bdf6`, prior commit, deferred to whole-plan review)
- Task 02 (`02-rc04-outbox-claim-lease-fetch-failed-sql`): complete (`5c1bdf6..dcc0f07`, prior commit, deferred to whole-plan review)
- Task 03 (`03-rc04-outbox-mark-failed-recursion-pool-acquire`): complete (`dcc0f07..cda711f`, prior commit, deferred to whole-plan review)
- Task 04 (`04-rc06-tenant-quota-sliding-window-ttl`): complete (`cda711f..b8e7f9b`, prior commit, deferred to whole-plan review)
- Task 05 (`05-rc01-singleflight-lost-wakeup`): complete (`b8e7f9b..0c5b2df`, prior commit, deferred to whole-plan review)
- Task 06 (`06-rc01-circuit-breaker-halfopen`): complete (`0c5b2df..728b107`, prior commit, deferred to whole-plan review)
- Task 07 (`07-rc01-builder-shutdown-wakeup`): complete (`728b107..247be32`, prior commit, deferred to whole-plan review)
- Task 08 (`08-rc07-session-tenant-id-regenerate`): complete (`247be32..4992fa1`, prior commit, deferred to whole-plan review)
- Task 09 (`09-rc15-csrf-layer-secret`): complete (`4992fa1..657a863`, prior commit, deferred to whole-plan review)
- Task 10 (`10-rc14-safe-equal`): complete (`657a863..1f3b2e4`, `a6605fa..9fda6d0`, prior commit, deferred to whole-plan review)
- Task 11 (`11-rc03-env-loading`): complete (`1f3b2e4..79448a1`, prior commit, deferred to whole-plan review)
- Task 12 (`12-rc05-builder-and-error`): complete (`79448a1..a8da219`, prior commit, deferred to whole-plan review)
- Task 13 (`13-rc08-rate-limiter`): complete (`a8da219..d0ebda4`, prior commit, deferred to whole-plan review)
- Task 14 (`14-rc09-metrics`): complete (`d0ebda4..f91b58f`, prior commit, deferred to whole-plan review)
- Task 15 (`15-rc10-saml`): complete (`f91b58f..9123928`, prior commit, deferred to whole-plan review)
- Task 16 (`16-rc11-macro-and-cli-db`): complete (`f98b6c6..55b7124`, review clean; 2 minors)
- Task 16: minor (deferred): silent error fallback in `resolve_db_url_in` on `EnvError`/`ConfigError::Validation` (`crates/cli/src/commands/db/mod.rs`)
- Task 16: minor (deferred): ambient `DATABASE_URL` sensitivity in `resolve_db_url_uses_env_interpolation_and_config_layering` (`crates/cli/src/commands/db/mod.rs`)
- Task 17 (`17-rc12-sql-check`): complete (`9123928..a6605fa`, prior commit, deferred to whole-plan review)
- Task 18 (`18-rc13-persistence-transaction`): complete (`55b7124..50fbfc5`, review clean; 3 minors)
- Task 18: minor (deferred): pre-existing parameter-binding duplication in MySQL and SQLite transactions (`crates/core/src/persistence/mysql.rs`, `sqlite.rs`)
- Task 18: minor (deferred): redundant `execute`/`query_scalar` trait method overrides matching `DatabaseTransaction` defaults (`postgres.rs`, `mysql.rs`, `sqlite.rs`)
- Task 18: minor (deferred): minor test coverage asymmetry across backends (`postgres.rs`, `sqlite.rs`)
- Task 19 (`19-rc16-saga`): complete (`50fbfc5..0380600`, review clean; 1 minor)
- Task 19: minor (deferred): `panic_message` parameter borrows `&Box<dyn Any + Send>` instead of `&(dyn Any + Send)` (`crates/core/src/saga/coordinator.rs:569`)
- Task 20 (`20-rc17-lock-renew`): complete (`0380600..3eec7a0`, review clean; 1 minor)
- Task 20: minor (deferred): no explicit `expires_at` assertion added in backend container tests (`coordination.rs`, `postgres.rs`, `redis.rs`)
- Task 21 (`21-rc18-testing-server`): complete (`3eec7a0..2f5099b`, review clean)
- Task 22 (`22-rc20-doctor-dag`): complete (`9fda6d0..f98b6c6`, prior commit, deferred to whole-plan review)
- Task 23 (`23-rc19-rc21-code-hygiene`): complete (`2f5099b..c36d5c3`, review clean; 2 minors; live MySQL container suite 12/12 passed resolving cannot-verify item)
- Task 23: minor (deferred): tight 40ms TTL in `health_check_cache_ttl_edge_cases_match_contract` (`crates/core/src/routing/health.rs:1233-1257`)
- Task 23: minor (deferred): trailing semicolon in reused `mysql_migration_unlock.sql` (`crates/core/src/persistence/sql/mysql/mysql_migration_unlock.sql`)

## Rulings

- Ruling: Tasks 01–15, 17, and 22 were already committed (`fce5006..f98b6c6`) before this `plan-executor` run; mark them complete and defer their review to the whole-plan review (`fce5006..HEAD`) rather than re-implementing them. — Avoids overwriting committed fixes while ensuring full review coverage via `review-orchestration`. — Costs rework only if a prior commit has a defect, which the whole-plan review will surface.
