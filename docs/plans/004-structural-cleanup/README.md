# Plan 004 — Structural cleanup

## Goal

Close the structural goals of the `rume` workspace while keeping every phase runnable end to
end (each phase ends with `cargo build` green and its test gates green):

1. **No oversized monolithic files** — every file in `crates/` that is ≥300 lines and mixes
   responsibilities is split into modules by area (**Phase 1**).
2. **Max directory depth 4** — the tree satisfies
   `find crates -name '*.sql' | awk -F/ '{print NF}' | sort -n | tail -1` = deepest directory
   4, i.e. every SQL asset lives at `crates/data/sql/<dialect>/<name>.sql` (**Phase 2**).
3. **No production dump folders** — `crates/foundation/src/helpers/` → `crates/foundation/src/util/`;
   `util/html.rs` split when it mixes text/HTML concerns; empty `./tmp/` deleted;
   `crates/testing/src/fixtures.rs` (777) split into area modules under
   `crates/testing/src/fixtures/` (**Phase 3**).
4. **No cross-crate SQL duplication** — the SQL tree exists once, at `crates/data/sql/`;
   `rume_data::sql` is the module; messaging SQL is consumed via `rume_data::sql::…` consts;
   `crates/data/src/persistence/sql{,.rs}` deleted (**Phase 2**).
5. **No naming drift** — post-split names follow the surrounding conventions; remaining
   drift is listed under "Flagged gaps" and "Phase 4 note".

All phases are pure refactors: **no behavior changes, no Cargo.toml dependency changes**, and
`cargo build` stays green after every task (phase-level: after every phase).

## Current state (read before executing)

Facts verified against the tree at plan time:

- **The 300+ band** (`find crates -name '*.rs' -not -path '*/target/*' -exec wc -l {} + | awk '$1 >= 300' | sort -rn`): 78 files, top of the list:
  `crates/messaging/src/jobs/worker.rs` (3,286), `crates/enterprise/src/saga/coordinator.rs` (2,411),
  `crates/app/src/app/builder.rs` (2,359), `crates/enterprise/src/enterprise/saml.rs` (1,991),
  `crates/core/tests/containers/postgres.rs` (1,931), `crates/data/src/persistence/migration.rs`
  (1,817), `crates/data/src/persistence/backend.rs` (1,463), `crates/core/tests/integration/outbox.rs`
  (1,322), … down to `crates/foundation/src/helpers/html.rs` (324).
- **SQL trees**:
  - `crates/data/src/persistence/sql{,.rs}` — 50 `.sql` files today; consts `PG_JOBS_SCHEMA`,
    `PG_AUDIT_SCHEMA`, `MYSQL_AUDIT_SCHEMA`, `SQLITE_AUDIT_SCHEMA`, `AUDIT_COUNT_SQL`,
    `MYSQL_SET_ISOLATION_LEVEL` defined in `crates/data/src/persistence/sql.rs`
    (`rume_data::persistence::sql::*`; `PG_JOBS_SCHEMA` is already consumed by messaging via
    `rume_data::persistence::sql::PG_JOBS_SCHEMA` at `crates/messaging/src/jobs/postgres.rs:23`).
  - `crates/messaging/src/jobs/sql/postgres/*.sql` — 14 files (jobs_ack, jobs_claim,
    jobs_dead_letter, jobs_dead_letter_count, jobs_defer, jobs_idempotency_check, jobs_insert,
    jobs_list_dead_letters, jobs_nack, jobs_pending_count, jobs_purge, jobs_reap,
    jobs_renew_lease, jobs_replay_dead_letter); consumed via `include_str!("sql/postgres/…")`
    in `crates/messaging/src/jobs/postgres.rs`.
  - `crates/messaging/src/outbox/sql/{mysql,postgres,sqlite}/*.sql` — 16+15+14 files;
    consumed via `include_str!("sql/…")` in `crates/messaging/src/outbox/{mysql,postgres,sqlite}.rs`.
  - `workbench/multi-instance/sql/postgres/{issues,organizations,projects,users}/…` — out of
    scope; left as-is.
  - **Verification of Phase 2 (conductor question 2):** diffing every messaging SQL file
    against same-named files in the data tree produced no `IDENTICAL`/`DIFFERS` pairs — the
    messaging trees are not covered by the data tree; all 59 files are distinct assets. None
    are "already covered"; all are moved into `crates/data/sql/` (no deletions of messaging
    trees; the data tree keeps both its own files and the messaging-only queries).
- **Dump folders**: `crates/foundation/src/helpers/` (bytes, case, duration, html, mask,
  ordinal, sentence, slug, truncate, whitespace, safe_equal + `mod.rs`); empty `./tmp/` (0
  entries); `crates/testing/src/fixtures.rs` (777).
- **Depth today**: `find crates -name '*.sql' | awk -F/ '{print NF}' | sort -n | uniq -c` →
  `3@4, 4@6, 110@7`; deepest SQL paths, e.g. `crates/messaging/src/outbox/sql/sqlite/outbox_select_retry_count.sql`
  (6 dirs). Target after Phase 2: every tree at `crates/data/sql/<dialect>/…` (4 dirs).
  (`crates/macro/tests/fixtures/**` SQL fixtures sit at depth 5 and are flagged in "Flagged
  gaps"; they are test data, not production assets.)
- **Coupling graph** (from manifests, not restated here): foundation→(observability, macro);
  data→(foundation, observability); messaging→(foundation, observability, data);
  security→(foundation, data); http→(foundation); enterprise→(foundation, observability);
  app→(foundation, observability, http); testing→(foundation, observability, data, messaging,
  http, app); core("rume")→all of the above + testing; cli→core; crash→core;
  workbench/*→core. No cycles; `rume-data` is the shared engine crate (`rume_data::`).

## Global constraints

- Pure refactor: **no behavior changes** anywhere in `crates/`; **no `Cargo.toml` dependency
  changes** (no new crates; feature flags untouched).
- `cargo build` must be green after every task and after every phase.
- Per-task gates (Phase 1 and Phase 3): `cargo build` + the touched crate's
  `cargo test -p <touched crate>` green (each task file lists its own).
- Phase 2 gate: `cargo build` + `cargo test -p rume-data -p rume-messaging -p rume-core` green;
  max SQL depth ≤ 4 (`find crates/data/sql -type d | awk -F/ '{print NF}' | sort -n | tail -1`);
  `find crates/messaging -name '*.sql'` empty; `grep -rn "persistence::sql" crates --include=*.rs`
  empty.
- Out of scope: `workbench/`, `lab/`, `fuzz/`, `benchmarks/load/`, `docs/` (except this plan
  folder). No commit steps unless the user asks.

## Constraints honoured (by phase)

- **Phase 1** honours the conductor's decisions: Phase 1 scope = the whole 300+ band (all 78
  files get a disposition: split task or "cohesive singleton — no task"); user proposals for
  the flagship splits are adopted where reading confirms mixed responsibilities (deviations
  documented per task).
- **Phase 2** honours the conductor's decisions: SQL assets centralized in `crates/data/sql/`
  as the single shared home; `crates/data/src/persistence/sql{,.rs}` deleted; `rume-data` is
  the home of all SQL (`rume_data::sql::…`); messaging includes updated to reference the new
  `rume_data::sql::…` locations. Global constraints as above.
- **Phase 3** honours the conductor's decisions: `helpers/` → `util/`; `util/html.rs` split
  (it mixes HTML escape/strip with text excerpt — confirmed by reading); empty `./tmp/`
  deleted; `crates/testing/src/fixtures.rs` split into per-area modules under
  `crates/testing/src/fixtures/`.

## Task index

| Task | File | Summary |
|---|---|---|
| 01 | `01-worker-split.md` | Split `crates/messaging/src/jobs/worker.rs` → `worker/{mod,registry,runner,heartbeat,tests}.rs` |
| 02 | `02-saga-coordinator-split.md` | Split `crates/enterprise/src/saga/coordinator.rs` → `coordinator/{mod,store,workflow,tests}.rs` |
| 03 | `03-app-builder-split.md` | Split `crates/app/src/app/builder.rs` → `app/builder/{mod,middleware,router,tests}.rs` |
| 04 | `04-saml-module-split.md` | Split `crates/enterprise/src/enterprise/saml.rs` → `enterprise/src/saml/{mod,assertion,verify,discovery,tests}.rs` |
| 05 | `05-migration-split.md` | Split `crates/data/src/persistence/migration.rs` → `migration/{mod,preflight,runner,tests}.rs` |
| 06 | `06-persistence-backend-split.md` | Split `crates/data/src/persistence/backend.rs` → `persistence/backend/{mod,validation}.rs` |
| 07 | `07-jobs-redis-split.md` | Split `crates/messaging/src/jobs/redis.rs` → `jobs/redis/{mod,heartbeat,idempotency}.rs` |
| 08 | `08-coordination-redis-split.md` | Split `crates/data/src/coordination/redis.rs` → `coordination/redis/{mod,lock,leader}.rs` |
| 09 | `09-sql-tree-centralization.md` | Move SQL tree to `crates/data/sql/`, delete `persistence/sql{,.rs}`, create `rume_data::sql`, update all consumers |
| 10 | `10-sql-centralization-verify.md` | Phase 2 verification: build + tests + depth + no leftover trees |
| 11 | `11-helpers-to-util-rename.md` | Rename `foundation/src/helpers/` → `util/`, update `rume_foundation::helpers`/`rume::helpers` references |
| 12 | `12-split-util-html.md` | Split `foundation/src/util/html.rs` into `util/html/{mod,escape,strip,excerpt}.rs` |
| 13 | `13-delete-empty-tmp.md` | Delete the empty `./tmp/` directory |
| 14 | `14-split-testing-fixtures.md` | Split `testing/src/fixtures.rs` into `fixtures/{mod,transaction,sqlite,postgres,tests}.rs` |

Each phase ends runnable: Phase 1 = splits with `cargo build` + `cargo test -p <touched>` green
after every task; Phase 2 = green as gated above; Phase 3 = splits with builds/tests green.

## Done-when checklist

- [ ] **Phase 1:** all 8 splits done; no band file in `crates/` remains unsplit except the
  cohesive singletons listed in "Observations (cohesive singletons — no task)" plus
  `foundation/src/helpers/html.rs` (324, split in Phase 3 task 12) and
  `testing/src/fixtures.rs` (777, split in Phase 3 task 14); `cargo build` + per-task
  `cargo test -p <touched crate>` green after every task.
- [ ] **Phase 2:** `crates/data/sql/` holds the single SQL tree (data + messaging trees merged,
  `workbench/` untouched); `crates/data/src/persistence/sql{,.rs}` gone; `rume_data::sql` module
  exists (`crates/data/src/sql.rs`); `rume_data::persistence::sql::` references gone from all of
  `crates/`; `find crates/messaging -name '*.sql'` empty; `cargo build` + `cargo test -p rume-data
  -p rume-messaging -p rume-core` green; `find crates/data/sql -type d | awk -F/ '{print NF}' |
  sort -n | tail -1` ≤ 4.
- [ ] **Phase 3:** `crates_foundation::util` + `rume::util` exist (via `foundation/src/lib.rs`
  `pub mod util;` + `core/src/lib.rs` `pub use rume_foundation::util;`) and all `rume_foundation::helpers`/
  `rume::helpers` references updated; `util/html/` split; `tmp/` deleted;
  `rume_testing::fixtures/` split; builds/tests green.

## Verification commands

- Per Phase 1/3 task: the `Verification` block of each task file (`cargo build` + `cargo test -p
  <touched crate>`).
- Phase 2: `cargo build`; `cargo test -p rume-data -p rume-messaging -p rume-core`;
  `find crates/data/sql -type d | awk -F/ '{print NF}' | sort -n | tail -1` → ≤ 4;
  `find crates/messaging -name '*.sql'` → empty; `grep -rn "persistence::sql" crates --include=*.rs` → empty.

## Flagged gaps

- `crates/macro/tests/fixtures/**` — SQL fixtures of the `rume-macros` migration macro, at
  depth-5 directories (`crates/macro/tests/fixtures/duplicate_migrations/*.sql`, etc.).
  Test-only data, not production SQL; flattening is a possible future task (out of scope now).
- `crates/crash/sql/*.sql` (3 files) — small SQL assets of the `rume-crash` crate; not in the
  conductor's four-copy list; left as-is (note for a possible future consolidation).
- Doc mentions of `helpers/` (e.g. `docs/plans/002-modular-core-and-otel/01-workspace-pins-and-foundation.md`
  "`crates/foundation/src/helpers/` …", `docs/plans/003-audit-remediation-and-hardening/05-security-path-saml-session-and-helpers.md`,
  `docs/plans/001-fix-audit-findings/10-rc14-safe-equal.md`) — `docs/` is out of scope; note for
  a future docs-hygiene pass.
- **Phase 4 note (on reading):** `crates/enterprise/src/enterprise/scim.rs` still nests under
  the odd `src/enterprise/` path (saml was moved to `crates/enterprise/src/saml/` by Phase 1
  task 04); consider moving `scim` to `crates/enterprise/src/scim/` in Phase 4.
- The claimed stale loose files at `docs/plans/` root (`01-06-*.md`, `README.md`, `progress.md`):
  **none exist in the tree today** — `docs/plans/` contains only `001-fix-audit-findings/`,
  `002-modular-core-and-otel/`, `003-audit-remediation-and-hardening/` (no loose files at root).
  Flag to the conductor if they expected different files.

## Answers to the conductor's open questions

1. **Phase 1 task decomposition (judged per file by reading):** 8 Phase-1 splits (tasks
   01–08); `foundation/src/helpers/html.rs` (324) splits in Phase 3 task 12;
   `testing/src/fixtures.rs` (777) splits in Phase 3 task 14; the remaining 68 band files are
   cohesive singletons — no split, observations listed below.
   Judgments on the user's proposed splits:
   - `worker.rs` (3,286): SPLIT — `JobRegistry` + `ErasedHandler` + `InFlightGuard` (registry.rs);
     `JobWorker`/`WorkerStatus`/`ActiveTaskGuard`/`JobWorker::{start, run_loop, execute_job}`
     (runner.rs); the per-job lease-heartbeat spawn + `HeartbeatAbortGuard` +
     `release_idempotency_token` (heartbeat.rs); `WorkerHandle` + `await_supervised_task` +
     `mod tests` (mod.rs — tests need access to `WorkerHandle` private fields, which are
     descendants of `worker`).
   - `saga/coordinator.rs` (2,411): SPLIT — `SagaStatus`/`SagaRecord`/`SagaStore`/`InMemorySagaStore`/
     `ExecutionClaimGuard`/`BoxFuture` (store.rs); `Saga`/`SagaReport`/`SagaRecoveryHandle`/
     `SagaRecoveryError`/`SagaRecoveryPhase`/`forward_idempotency_key`/`compensation_idempotency_key`/
     `attach_saga_id`/`panic_message` + `Saga` impl (workflow.rs); re-exports via mod.rs so
     `saga/mod.rs` and all consumers are unchanged.
   - `app/builder.rs` (2,359): SPLIT — mod.rs (`Rume`/`RumeBuilder` struct+core methods,
     `TestAppParts`, `ReadyHook`/`ShutdownHook`, `PreparedServe`, `IntoBindAddr`,
     `IntoHookResult`, `mod tests`); middleware.rs (`MiddlewareConfigError` + Display/Error/From,
     `use_middleware`/`use_group`/`middleware`/`group`, `validate_middleware_config`); router.rs
     (route methods, `build_router*`, `collect_and_validate_routes`, `merge_module_router`,
     `parse_axum_merge_conflict`, `normalize_route_template`, `methods_conflict`,
     `check_and_record_route`, `BuiltRouterWithCtx`).
   - `enterprise/saml.rs` (1,991): SPLIT — assertion.rs (`SamlParser` + `parse_internal`/
     `parse_unverified`/`parse_signed`/`parse_with_verifier`, `SamlAssertion`,
     `SamlSubjectConfirmationData`, `XmlNs`, `AssertionFields`, `TextTarget`, `ActiveTextCapture`,
     `is_ns`, `resolve_general_ref`, `parse_iso8601_strict`); discovery.rs (`SamlParser::parse_entity_id`);
     verify.rs (`SamlError`, `SamlValidationConfig`, `SamlSignatureVerifier`, `SamlReplayCache`,
     `InMemorySamlReplayCache`, `SamlParser::{validate, validate_strict, validate_with_config}`);
     mod.rs (re-exports). `enterprise/mod.rs` re-exports updated to `crate::saml::…`; `lib.rs` gains
     `#[cfg(feature = "saml")] pub mod saml;` — so `rume::enterprise::SamlParser` still resolves.
   - `data/persistence/migration.rs` (1,817): SPLIT — preflight.rs (all SQL consts,
     `migrations_schema_ddl`, `CREATE_MIGRATIONS_TABLE_DDL`, per-dialect query consts,
     `find_checksum_query`/`insert_migration_query`/`delete_migration_query`/
     `find_migration_id_query`, `LIST_APPLIED_MIGRATIONS_SQL`/`NEXT_APPLIED_ORDER_SQL`/
     `APPLIED_ORDER_MYSQL_COUNT_SQL`/`APPLIED_ORDER_SQLITE_COUNT_SQL`/`LIST_APPLIED_ORDER_SQL`,
     `select_rollback_target`, `ensure_applied_order_column_{tx,backend,mysql}`,
     `next_applied_order_{tx,mysql}`, `fetch_applied_order_rows_{tx,mysql}`, `parse_clock_or_now`);
     runner.rs (`Migration`, `MigrationSummary`, `Migrator` + `with_rume_core`/`add_migration`/
     `migrations`/`pending_migrations`/`applied_migrations`/`run_pending`/`rollback_last`/
     `verify_no_pending`/`verify_applied_checksums`/`run_pending_backend`/`rollback_last_backend`
     + MySQL session helpers `MysqlMigrationLockGuard`, `MysqlLockState`, `mysql_conn_*`,
     `bind_mysql_conn_params`, `extract_mysql_scalar`, `mysql_rollback_then_unlock`);
     mod.rs (re-exports; `RUME_CORE_MIGRATION_ID`/`RUME_AUDIT_MIGRATION_ID` re-exported so
     `rume::persistence::migration::RUME_CORE_MIGRATION_ID` still resolves).
   - `data/persistence/backend.rs` (1,463): SPLIT — mod.rs (`BoxFuture`, `DbValue` + all `From`
     impls, `params!` macro, `DatabaseBackend`, `DatabaseTransaction`, `map_sqlx_error`,
     `fuzz_support` re-export, DbValue + map_sqlx_error unit tests); validation.rs (the SQL
     validation machinery: `validate_managed_transaction_sql`,
     `validate_managed_mysql_transaction_sql`, `mysql_sql_causes_implicit_commit`,
     `sanitize_non_nested_block_comments`, `tokenize_sql_filtered`,
     `collect_top_level_statement_heads_from_tokens`, `check_ast_tx_control`,
     `is_mysql_implicit_commit_{ast,head}`, `InheritedSetScope`, `evaluate_mysql_set_lhs`,
     `check_mysql_autocommit_assignment_tokens`, `check_managed_tx_control_keywords`,
     `fuzz_support`, and their tests + proptests); mod.rs re-exports
     `pub(crate) use validation::{validate_managed_transaction_sql, validate_managed_mysql_transaction_sql,
     mysql_sql_causes_implicit_commit, sanitize_non_nested_block_comments}` so callers
     (`crates/data/src/persistence/{postgres,mysql,sqlite}.rs`, `migration/runner.rs`) keep
     compiling unchanged at `crate::persistence::backend::X`.
   - `messaging/jobs/redis.rs` (490): SPLIT — redis/mod.rs (re-exports); heartbeat.rs
     (`RedisHeartbeatRegistry` + `HeartbeatRegistry` impl: `record_heartbeat`, `is_alive`,
     `reap_dead_workers`); idempotency.rs (`RedisIdempotencyStore` + `IdempotencyStore` impl:
     `try_acquire{,_with_token}`, `complete{,_with_token}`, `release{,_with_token}`,
     `extend_lock_with_token`, `scoped_key` + private `key`/`hex_encode`/`hex_decode`).
   - `data/coordination/redis.rs` (464): SPLIT — redis/mod.rs (re-exports); lock.rs
     (`RedisDistributedLock` + `DistributedLock` impl: `acquire`, `renew`, `release`); leader.rs
     (`RedisLeaderElector` + `LeaderElector` impl: `try_acquire`, `renew`, `get_leader`,
     `resign`). `coordination/mod.rs`:9 keeps `pub use self::redis::{RedisDistributedLock, RedisLeaderElector}`
     (resolves via mod.rs re-exports).
2. **Phase 2 verification (conductor question 2):** none of the messaging SQL files are
   "already covered" by the data tree — all 59 are unique (verified by name against every data
   tree file and by `diff -q` content comparison against same-named data files: no
   `IDENTICAL`/`DIFFERS` pairs). All are moved into `crates/data/sql/` (new consts in
   `rume_data::sql`); `PG_JOBS_SCHEMA` was already shared (single copy in data, consumed via
   `rume_data::persistence::sql::PG_JOBS_SCHEMA` today).
3. **Naming (conductor question 3):** `util/` — `crates/foundation/src/helpers/` →
   `crates/foundation/src/util/`; public path becomes `rume_foundation::util`; core re-export
   becomes `rume::util`; internal `super::` refs unchanged. `html.rs` splits to
   `util/html/{mod,escape,strip,excerpt}.rs`. Doc-comment examples in helpers files are updated
   to `rume_foundation::util::…` (docs text otherwise untouched per scope).
4. **workbench/multi-instance/sql (conductor question 4):** left as-is (user decision 2:
   app-specific example-app queries; moving them into `data` would mix app queries with engine
   SQL).
5. **Discoveries (conductor question 5):** none that materially change the plan. New hotspots:
   none; hazards: the `pub(crate) use` re-export bridge in `backend/mod.rs` is the main
   mechanical risk of Phase 1 task 06; `worker/mod.rs` holding `WorkerHandle` + tests is a
   deliberate access-visibility decision. Test files (`crates/core/tests/**`,
   `crates/cli/tests/generated_project.rs`) are noted as cohesive singletons; some (postgres,
   outbox, pipeline, persistence, saga) exceed 500 lines but stay as single test targets —
   splitting test targets has little structural value.

## Observations (cohesive singletons — no task)

*No split proposed; each file is moved/observed as-is. "Vertical only" means the file sits at
its current path with its current name/size.*

**Messaging:**
- `crates/messaging/src/outbox/relay.rs` (822) — `CancellableMessageDispatcher`, `RelayPollResult`,
  `RelayStatus`, `OutboxSweeperConfig`, `OutboxRelayConfig` — cohesive outbox-relay config +
  poll result types.
- `crates/messaging/src/outbox/repository.rs` (459) — `OutboxRepository` trait + `TransactionalOutbox`
  + `DatabasePool` impls + tests — cohesive repository module.
- `crates/messaging/src/outbox/postgres.rs` (465) — dialect-specific `OutboxRepository`/
  `TransactionalOutbox` impl for `PostgresBackend` + row parser + SQL consts; the SQL consts move
  to `rume_data::sql` in Phase 2 (task 09).
- `crates/messaging/src/outbox/mysql.rs` (464) — same for `MysqlBackend` (SQL consts move in Phase 2).
- `crates/messaging/src/outbox/sqlite.rs` (557) — same for `SqliteBackend` (SQL consts move in Phase 2).
- `crates/messaging/src/jobs/postgres.rs` (529) — `PostgresJobQueue` + `current_epoch_millis`; SQL
  consts move to `rume_data::sql` in Phase 2 (task 09).
- `crates/messaging/src/events/bus.rs` (358) — `EventBus` — cohesive event bus.

**Security:**
- `crates/security/src/security/csrf.rs` (486) — `CsrfConfig`/`CsrfLayer`/`CsrfService` — cohesive.
- `crates/security/src/security/headers.rs` (474) — `SecurityHeadersConfig`/`SecurityHeadersLayer`/
  `SecurityHeadersService` — cohesive.
- `crates/security/src/tenant/cache.rs` (1057) — `TenantScopedCache` + `KeyLockGuard` +
  `TenantLockStripes` + key/tag scoping — cohesive tenant-cache wrapper with key-lock serialization.
- `crates/security/src/tenant/quota.rs` (792) — `TenantQuotaStore` + `InMemoryTenantQuotaStore` + `LeaseId` — cohesive quota store.
- `crates/security/src/tenant/layer.rs` (652) — `TenantLayer` + `TenantService` + `TenantMembershipLayer/Service` — cohesive.
- `crates/security/src/tenant/resolver.rs` (422) — `TenantResolver` trait + `HeaderTenantResolver` +
  `SubdomainTenantResolver` — cohesive trait + impls.

**HTTP:**
- `crates/http/src/routing/router.rs` (463) — `Router` + default/impls — cohesive core router type.
- `crates/http/src/routing/health.rs` (745) — `health_routes` — cohesive health-routes module.
- `crates/http/src/routing/spec.rs` (380) — `RouteSpec` + `ParamValidationLayer/Service` — cohesive.
- `crates/http/src/resilience/circuit_breaker.rs` (481) — `CircuitState`/`CircuitBreakerConfig`/
  `CircuitInner`/`TrialPermit`/`CircuitBreaker` — cohesive circuit-breaker state machine.
- `crates/http/src/resilience/rate_limiter.rs` (539) — `RateLimitConfig`/`RateLimiter`/`RateLimitLayer` — cohesive.
- `crates/http/src/validation.rs` (601) — `ValidatedJson`/`Validated<T>`/`ValidatedQuery`/`ValidatedPath`/
  `ValidatedForm` — cohesive validation request types.
- `crates/http/src/openapi/spec.rs` (597) — `OpenApiSpec` builder — cohesive.

**Foundation:**
- `crates/foundation/src/config.rs` (423) — `ConfigKeyExplanation`/`ConfigSourceMap`/`flatten_toml`/
  `ConfigError` — cohesive config parsing.
- `crates/foundation/src/error.rs` (643) — problem-details error types — cohesive.
- `crates/foundation/src/health.rs` (725) — `HealthChecks` + impls — cohesive.
- `crates/foundation/src/env/loading.rs` (745) — `ResolvedEnv` + env loading pipeline — cohesive.
- `crates/foundation/src/env/secrets.rs` (331) — secret-key detection + redaction helpers — cohesive.
- `crates/foundation/src/helpers/html.rs` (324) — SPLIT (Phase 3, task 12): mixes HTML concerns
  (`escape_html`, `strip_html_tags`) with a text concern (`excerpt`).

**App:**
- `crates/app/src/app/lifecycle.rs` (448) — `ResolvedModules` + `LifecycleError` +
  `resolve_modules` (topological module ordering; used by `app::builder`, `app::lifecycle`,
  `rume-testing`) — cohesive.

**Observability:**
- `crates/observability/src/metrics.rs` (1003) — `MetricsRegistry` + exporters — cohesive.
- `crates/observability/src/layer.rs` (821) — `ObservabilityLayer`/`ObservabilityService` — cohesive.
- `crates/observability/src/otel.rs` (379) — `ObservabilityGuard`/`ObservabilityBuilder` + init — cohesive.
- `crates/observability/src/trace_context.rs` (314) — `TraceContext` — cohesive.

**Data:**
- `crates/data/src/cache/typed.rs` (974) — `Cache<B>` + `SingleflightState`/`FlightSlot` — cohesive typed
  cache with single-flight.
- `crates/data/src/cache/backend.rs` (351) — `CacheBackend` trait + `InMemoryCacheBackend` + `TagIndex` — cohesive.
- `crates/data/src/cache/redis.rs` (381) — `RedisCacheBackend` — cohesive.
- `crates/data/src/coordination/postgres.rs` (340) — PG coordination (locks, leader, C2L) — cohesive.
- `crates/data/src/persistence/sqlite.rs` (878) — `SqliteBackend` — cohesive.
- `crates/data/src/persistence/postgres.rs` (586) — `PostgresBackend` — cohesive.
- `crates/data/src/persistence/mysql.rs` (595) — `MysqlBackend` — cohesive.
- `crates/data/src/persistence/pool.rs` (611) — `DatabasePool` + `ConnectionLease` +
  `DatabaseHealthCheck` — cohesive.
- `crates/data/src/persistence/row.rs` (517) — `FromDbValue` trait + impls — cohesive.
- `crates/data/src/persistence/backend.rs` — SPLIT (Phase 1, task 06).

**Enterprise:** (no notes — both split tasks consume)

**CLI:**
- `crates/cli/src/commands/doctor.rs` (831) — `DoctorOutput`/`DoctorCheck` + checks — cohesive.
- `crates/cli/src/commands/db/mod.rs` (795) — migration discovery + db-URL resolution — cohesive.
- `crates/cli/src/commands/sql_check.rs` (578) — SQL checker — cohesive.
- `crates/cli/src/commands/new.rs` (381) — `NewError` + scaffold logic — cohesive.
- `crates/cli/tests/generated_project.rs` (563) — generated-project integration test module — cohesive.

**Testing:**
- `crates/testing/src/app.rs` (517) — `TestApp`/`TestAppBuilder` — cohesive.
- `crates/testing/src/server.rs` (345) — `TestServer` — cohesive.
- `crates/testing/src/conformance.rs` (386) — `ConformanceRecord` — cohesive.
- `crates/testing/src/fixtures.rs` (777) — SPLIT (Phase 3, task 14) into area modules.

**Core tests (cohesive test scenarios; no split):**
- `crates/core/tests/containers/postgres.rs` (1,931)
- `crates/core/tests/containers/pipeline.rs` (1,094)
- `crates/core/tests/containers/redis.rs` (872)
- `crates/core/tests/containers/mysql.rs` (689)
- `crates/core/tests/containers/jobs.rs` (501)
- `crates/core/tests/containers/crash_recovery.rs` (498)
- `crates/core/tests/containers/job_reliability.rs` (345)
- `crates/core/tests/integration/outbox.rs` (1,322)
- `crates/core/tests/integration/persistence.rs` (882)
- `crates/core/tests/integration/saga.rs` (868)
- `crates/core/tests/integration/enterprise.rs` (726)
- `crates/core/tests/integration/resilience.rs` (700)
- `crates/core/tests/integration/request_identity.rs` (646)
- `crates/core/tests/integration/composition.rs` (623)
- `crates/core/tests/integration/tenant.rs` (594)
- `crates/core/tests/integration/sqlite.rs` (549)
- `crates/core/tests/integration/auth.rs` (489)
- `crates/core/tests/integration/security.rs` (453)
- `crates/core/tests/integration/architecture.rs` (413)
- `crates/core/tests/integration/test_server.rs` (363)

Test targets are cohesive scenarios; splitting test files has little structural value (note:
the 6 largest exceed 500 lines but stay as single test crates/targets).

**Other:** `crates/macro/src/lib.rs` (694) — `sql_assets` + `embed_migrations` proc macros + shared
helpers — cohesive macro crate (sql_assets/embed_migrations share most helpers; splitting a
proc-macro crate is possible but low value — deferred to Phase 4 note if desired).
