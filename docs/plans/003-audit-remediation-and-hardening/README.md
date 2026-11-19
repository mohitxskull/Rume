# Plan 003 — Audit Remediation & Hardening

## Goal

Remediate all confirmed concurrency, migration, SQL/locking, security, packaging, lifecycle, observability, testing, and CLI scaffolding defects identified on `main` (`31683f6`):

1. **Eliminate concurrency & RAII leaks (`RC-01`):** Make `Cache::remember_with_tags` singleflight cancellation/panic-safe with RAII `SingleflightLeaderGuard` + follower re-contention loop, and replace manual `JobWorker` `in_flight_handles` / `in_flight_seq` / `InFlightHandleGuard` tracking with `tokio::task::JoinSet`.
2. **Preserve core migration ledger & MySQL direct session execution (`RC-02`, `OQ-01`):** Stop dropping `rume_migrations` and `rume_audit_log` in `core_down.sql`, split audit schema into append-only `20261104_002_rume_audit_schema` while keeping `20261104_001_rume_core_schemas` checksum-immutable, and run MySQL migrations directly on a `GET_LOCK`-pinned `PoolConnection<MySql>` without fake multi-migration `START TRANSACTION` / `COMMIT` / `ROLLBACK` wrappers.
3. **Fix outbox & jobs SQL/locking races (`RC-03`):** Use SQLite `BEGIN IMMEDIATE` + `fetch_optional` in `claim_pending_with_lease`, select `trace_context` strictly in `PostgresJobQueue::list_dead_letters`, and decouple `fetch_pending` from `FOR UPDATE SKIP LOCKED` in Postgres/MySQL outbox.
4. **Harden security & validation boundaries (`RC-04`, `OQ-02`):** Fail closed on `fs::canonicalize` errors (`loading.rs`, `db/mod.rs`), replace hand-rolled SAML timestamp/offset parsing with `chrono::DateTime::parse_from_rfc3339`, harden SAML `signed_mode` reference binding, `<saml:Issuer>` anchoring, and stale `saml-rs` docs, mutate Redis sessions structurally via `cjson` instead of Lua patterns, and remove unused `safe_equal_hashed`.
5. **Enforce self-contained crate packaging & clean `rume` binary/deps (`RC-05`):** Eliminate all 22 cross-crate `include_str!("../../../data/...")` paths, move `rume-crash-harness` + `crash_*.sql` into a `publish = false` `crates/crash-harness` crate, and demote `tokio`/`sqlx`/`uuid` in `crates/core/Cargo.toml` to `[dev-dependencies]`.
6. **Synchronize `RumeBuilder` state, `Inspect` validation & lifecycle (`RC-06`):** Propagate finalized `route_registry` and `health_checks` into `TestAppParts.ctx`/`serve_ctx`, remove dead `state: S`, validate DAG + route collisions in `ApplicationMode::Inspect`, and consolidate lifecycle hooks in `rume-app`.
7. **Align tenant quota TTLs & Redis quota key hygiene (`RC-07`):** Resolve default cache TTL before `quota.record_key`, roll back quota slots on backend/closed cache errors, fix `RedisTenantQuotaStore::key()` double prefix, and maintain rolling `PEXPIRE`/`PERSIST` on the quota key.
8. **Fix W3C child `TraceContext` propagation & replace `PeriodicReader` (`RC-08`):** Emit and propagate the server span's child `TraceContext` in `ObservabilityLayer`, add `event.publish_sync` span + `Why:` comments on `EventBus`/`Saga`, replace `PeriodicReader` + `InMemoryMetricExporter` with `ManualReader` (`experimental_metrics_custom_reader`), and delete unused `Counter`/`Gauge`.
9. **Standardize persistence error taxonomy & `sqlparser`-backed SQL guards (`RC-09`):** Emit `PersistenceError::DecodeError` in `FromDbValue`, fix `From<usize> for DbValue`, rewrite `validate_managed_transaction_sql` and `reject_unbound_sqlite_placeholders` using `sqlparser = "=0.53.0"` across Postgres/MySQL/SQLite transactions, and deduplicate MySQL/SQLite parameter binding.
10. **Make test fixtures deterministic, add property tests, harden CLI & `#![forbid(unsafe_code)]` (`RC-10`, `RC-11`):** Make `PostgresSchemaFixture` and `TestTransactionFixture` `Drop` cleanup reactor-safe without `sleep(50ms)` (fresh-connection helper/`block_in_place` for `PostgresSchemaFixture`; `block_in_place` on `multi_thread` or helper thread for SQLite in `TestTransactionFixture`), add `proptest` suites (`rume-enterprise`, `rume-data`, `rume-foundation`), validate `--dialect` via `ValueEnum` and align `rume new` SQL templates with `rume sql:check`, and add `#![forbid(unsafe_code)]` to `crates/macro` and `crates/xtask`.

## Current state (read before executing)

- `crates/data/src/cache/typed.rs:23,167-208` uses `tokio::sync::Mutex` for `singleflight`, manually removes slots after `set_json_with_tags` (leaking slots on error/panic/cancellation), and falls through to `f().await` without holding a slot if `get_json` returns `None`.
- `crates/messaging/src/jobs/worker.rs:648-675` spawns the task before inserting `join_handle.abort_handle()` into `in_flight_handles`.
- `crates/data/src/persistence/sql/{postgres,mysql,sqlite}/core_down.sql` drop `rume_migrations` and `rume_audit_log`; `crates/data/src/persistence/migration.rs:183-206,381-480,627-693` registers only `20261104_001_rume_core_schemas` and wraps multi-migration MySQL DDL in an outer `START TRANSACTION`.
- `crates/messaging/src/outbox/sqlite.rs:131-166` uses deferred `pool().begin()` + `.fetch_one()`; `crates/messaging/src/outbox/{postgres,mysql}.rs` reuse `OUTBOX_CLAIM_SQL` (`FOR UPDATE SKIP LOCKED`) in `fetch_pending`; `crates/data/src/persistence/sql/postgres/jobs_list_dead_letters.sql` omits `trace_context` and `crates/messaging/src/jobs/postgres.rs:475` swallows errors via `.ok()`.
- `crates/foundation/src/env/loading.rs:367-382` and `crates/cli/src/commands/db/mod.rs:361-370` fall back to uncanonicalized paths on `fs::canonicalize` error and read `&full`.
- `crates/enterprise/src/enterprise/saml.rs:15-25,265-293,503-635` and `Cargo.toml:64-67` allow empty `signature_references`/missing `Assertion/@ID` in `signed_mode`, match `<Issuer>` without `grand_parent == Some(("Assertion", true))`, slice `s[..2]`/`s[2..]` on 4-byte UTF-8 without bounds checks (`0..=23`, `0..=59`), and overstate `saml-rs` usage.
- `crates/security/src/auth/redis.rs:17-85,209-230` uses Lua pattern/plain-find string replacement for `regenerate_id`; `crates/foundation/src/helpers/safe_equal.rs:53-58` retains unused `safe_equal_hashed`.
- 22 `include_str!("../../../data/...")` calls span `crates/core/src/bin/crash_harness.rs` (3), `crates/security/src/security/audit.rs` (4), `crates/messaging/src/outbox/mysql.rs` (1), and `crates/messaging/src/jobs/postgres.rs` (14); `crates/core/Cargo.toml` ships `[[bin]] name = "rume-crash-harness"` and runtime `tokio`/`sqlx`/`uuid`.
- `crates/app/src/app/builder.rs:282,596-615,1021-1158` passes stale `ctx` copies to `TestAppParts.ctx`/`serve_ctx`, retains dead `state: S`, skips DAG/route validation in `ApplicationMode::Inspect`, and duplicates lifecycle logic from `crates/testing/src/ext.rs:44-170`.
- `crates/security/src/tenant/cache.rs:104-135` passes raw `ttl` (`None`) to `quota.record_key` and skips rollback when `cache.exists` errors; `crates/security/src/tenant/quota.rs:279-299,332-334` formats `"rume:quota:quota:{...}:keys"` and omits `PEXPIRE` on `KEYS[1]`.
- `crates/observability/src/layer.rs:82-177` inserts/logs/returns the incoming caller `span_id`; `crates/observability/src/metrics.rs:17,52-92,191-193` uses `PeriodicReader` + `InMemoryMetricExporter` (`opentelemetry_sdk` `testing` feature) and keeps unused `Counter`/`Gauge`.
- `crates/data/src/persistence/row.rs:13-225` emits `QueryFailed` inside `FromDbValue`; `crates/data/src/persistence/backend.rs:113-117` truncates `usize` via `i64`; `crates/data/src/persistence/{mysql,sqlite}.rs` lack `validate_managed_transaction_sql` and repeat bind loops.
- `crates/testing/src/fixtures.rs:70-81,280-296,355` spawns async drop tasks and sleeps 50ms; `crates/cli/templates/sql/items/{create,find}.sql.tpl` use `$1..$3` while default `rume` uses SQLite; `crates/macro/src/lib.rs` and `crates/xtask/src/main.rs` lack `#![forbid(unsafe_code)]`.

## Global constraints

- **Work on `main` (`rules/30-git.md`):** No feature branches or worktrees; no commits without explicit user authorization.
- **Code + `Why:` comments (`rules/70-code-rationale.md`):** Every non-obvious decision (choice, rejected alternative & why it lost, revisit condition if any) MUST carry an inline `Why:` comment at the decision site.
- **Concrete pins (`rules/10-tech-stack.md`):** Any new or promoted `[workspace.dependencies]` entry MUST use an exact `=x.y.z` pin.
- **CPU-friendly verification:** Pass `-j 2` on all `cargo check`, `cargo clippy`, `cargo test`, and `cargo doc` commands.
- **Zero-warning bar (`rules/80-quality-bar.md`):** Every task must pass `cargo fmt --all --check` and `cargo clippy --workspace --all-targets --all-features -j 2 -- -D warnings`.
- **Explicitly OUT OF SCOPE:**
  - Do NOT alter `crates/foundation/src/env/loading.rs` interpolation (already uses `interpolation::interpolate_value`).
  - Do NOT reference removed Plan 002 items (`CURRENT_TRACE_CONTEXT`, `render_prometheus`).
  - Do NOT claim `URI=""` is accepted in SAML `signed_mode` (already rejected at `crates/enterprise/src/enterprise/saml.rs:590-596`).
  - Do NOT hand-roll `exc-c14n` or X.509 RSA-SHA256 in `saml.rs` (`OQ-02`).
  - Keep `.cargo/audit.toml`'s `RUSTSEC-2026-0459` ignore (`OQ-03`).

## Constraints honoured

- `rules/40-verification.md`, `rules/70-code-rationale.md`, `rules/80-quality-bar.md`, `rules/85-minimalism.md`, `rules/testing.md`.
- `.github/workflows/ci.yml` isolated feature matrix (`sqlite,testing`, `postgres,testing`, `mysql,testing`, `redis,testing`, `--all-features`).
- `crates/core/src/lib.rs` and `crates/core/src/prelude.rs` re-export contracts (except explicitly deleting unused `Counter`/`Gauge` and `safe_equal_hashed`).
- `crates/core/tests/integration/{persistence,sqlite,enterprise,request_identity,metrics,outbox,security,cache,tenant,property_tests}.rs`, `crates/core/tests/containers/{crash_recovery,postgres,mysql,redis}.rs`, `crates/cli/tests/{generated_project,cli_workflows}.rs`, and `workbench/multi-instance/tests/{deployment_validation,workbench_test}.rs`.

## Task index

- `01-concurrency-raii-guards.md` — Add RAII `SingleflightLeaderGuard` + follower re-contention in `Cache::remember_with_tags` and replace manual `JobWorker` handle tracking with `tokio::task::JoinSet` (`RC-01`).
- `02-core-migrations-and-mysql-atomicity.md` — Preserve `rume_migrations` across core rollbacks, split `rume_audit_log` into append-only `20261104_002_rume_audit_schema`, and run MySQL migrations directly on a `GET_LOCK`-pinned `PoolConnection<MySql>` (`RC-02`).
- `03-persistence-error-taxonomy-and-tx-guards.md` — Standardize `FromDbValue` `DecodeError`, fix `From<usize> for DbValue`, enforce `sqlparser`-backed `validate_managed_transaction_sql` & `reject_unbound_sqlite_placeholders`, and deduplicate bind helpers (`RC-09`).
- `04-outbox-and-jobs-sql-locking.md` — Acquire `BEGIN IMMEDIATE` + `fetch_optional` in SQLite outbox claiming, add non-locking `outbox_fetch_pending.sql` for Postgres/MySQL, and select/decode `trace_context` in Postgres DLQ listings (`RC-03`).
- `05-security-path-saml-session-and-helpers.md` — Fail closed on `fs::canonicalize`, replace manual SAML date/offset parsing with `chrono::DateTime::parse_from_rfc3339`, harden `signed_mode` references/`Issuer`/docs, switch Redis session rotation to structural `cjson`, and delete `safe_equal_hashed` (`RC-04`).
- `06-tenant-quota-ttl-and-redis-hygiene.md` — Resolve effective cache TTL before `quota.record_key`, roll back quota on backend/closed errors, fix `RedisTenantQuotaStore::key()` double prefix, and set rolling `PEXPIRE`/`PERSIST` (`RC-07`).
- `07-builder-state-sync-inspect-and-lifecycle.md` — Return finalized `AppContext` from router construction, remove dead `RumeBuilder::state`, validate DAG/routes in `Inspect` mode, and consolidate lifecycle hooks in `rume-app` (`RC-06`).
- `08-otel-trace-propagation-and-metrics-reader.md` — Propagate server child `TraceContext` in `ObservabilityLayer`, add `event.publish_sync` span + `Why:` notes, migrate `MetricsRegistry` to `ManualReader`, and delete `Counter`/`Gauge` (`RC-08`).
- `09-crate-packaging-and-crash-harness.md` — Eliminate all 22 cross-crate `include_str!("../../../data/...")` paths, move `rume-crash-harness` to `crates/crash-harness`, and demote `tokio`/`sqlx`/`uuid` to `[dev-dependencies]` in `crates/core` (`RC-05`).
- `10-deterministic-fixtures-proptests-and-cli-hardening.md` — Make `PostgresSchemaFixture` / `TestTransactionFixture` `Drop` reactor-safe without `sleep(50ms)`, add `proptest` suites, validate `--dialect` and align `rume new` SQL templates with `rume sql:check`, and forbid `unsafe` in `macro`/`xtask` (`RC-10`, `RC-11`).

## Done-when checklist

- [ ] `! rg -n 'include_str!\("\.\./\.\./\.\./data/' crates/` exits cleanly (0 cross-crate `include_str!` paths remain; intra-crate `crates/cli/src/commands/generate/*.rs` `../../../templates/src/` includes are expected).
- [ ] `! rg -n 'safe_equal_hashed|escape_lua_pattern|InFlightHandleGuard|parse_tz_offset' crates/` and `! rg -n 'InMemoryMetricExporter|PeriodicReader' crates/observability/src/metrics.rs` exit cleanly.
- [ ] `cargo package --list --allow-dirty` succeeds for every publishable workspace crate (`rume-foundation`, `rume-observability`, `rume-data`, `rume-messaging`, `rume-security`, `rume-http`, `rume-enterprise`, `rume-app`, `rume-testing`, `rume`, `rume-macros`, `rume-cli`).
- [ ] `cargo fmt --all --check` exits 0.
- [ ] `cargo clippy --workspace --all-targets --all-features -j 2 -- -D warnings` exits 0.
- [ ] `cargo test --workspace --lib --all-features -j 2`, `cargo test -p rume --test integration --all-features -j 2`, `cargo test -p rume-cli -j 2`, and isolated feature `cargo check` commands pass without skipped or failed assertions.
- [ ] Docker container suites (`cargo test -p rume --test containers --all-features -j 2` and `workbench/multi-instance` tests) pass when Docker services are available.

## Verification commands

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -j 2 -- -D warnings
cargo check -p rume --no-default-features --features sqlite,testing -j 2
cargo check -p rume --no-default-features --features postgres,testing -j 2
cargo check -p rume --no-default-features --features mysql,testing -j 2
cargo check -p rume --no-default-features --features redis,testing -j 2
cargo test --workspace --lib --all-features -j 2
cargo test -p rume --test integration --all-features -j 2
cargo test -p rume-cli -j 2
cargo test --workspace --doc --all-features -j 2
```

## Flagged gaps

- **`Migrator::split_sql_statements` does not exist on `main` (`31683f6`):** Migrations execute multi-statement SQL directly via `sqlx::raw_sql`. In Task 10 (`RC-10`), `rume-data` property-based tests therefore fuzz and verify the real custom SQL lexers/scanners present in `crates/data`: `validate_managed_transaction_sql` (moved to `crates/data/src/persistence/backend.rs` in Task 03) and `reject_unbound_sqlite_placeholders` (`crates/data/src/persistence/sqlite.rs`).
- **`SamlAssertionParser` naming:** The actual SAML parser struct in `crates/enterprise/src/enterprise/saml.rs:296` is `SamlParser` (not `SamlAssertionParser`); Task 05 and Task 10 target `SamlParser` and `chrono`-backed `parse_iso8601_strict`.
- **Existing tests updated for corrected behavior:**
  - `RC-02` adds a second core migration (`20261104_002_rume_audit_schema`), so `crates/core/tests/integration/persistence.rs:513-518` updates to `applied.len() == 3`, `applied[1] == RUME_AUDIT_MIGRATION_ID`, and `applied[2] == "20261104_002_custom_app"`, and `.applied.len()` increases by 1 in `crates/core/tests/integration/sqlite.rs:150`, `workbench/multi-instance/tests/deployment_validation.rs:88`, and `workbench/multi-instance/tests/workbench_test.rs:71`.
  - `RC-04` enforces `<ds:Reference>` coverage of `Assertion/@ID` in `signed_mode`, so `test_saml_signature_verifier` (`crates/core/tests/integration/enterprise.rs:153-171`) and `test_saml_trust_boundary_exact_assertion_verified` (`crates/core/tests/integration/enterprise.rs:473-494`) must include a matching `<ds:Signature><ds:SignedInfo><ds:Reference URI="#..." /></ds:SignedInfo></ds:Signature>`.
  - `RC-08` emits the server span's child `TraceContext` on responses, so `observability_layer_extracts_w3c_traceparent_into_otel_span_and_response` (`crates/observability/src/layer.rs:295-329`) and `test_w3c_trace_context_automatic_propagation_across_http_outbox_jobs_and_worker` (`crates/core/tests/integration/request_identity.rs:420-490`) capture `handler_tc = TraceContext::current()` inside the handler and assert preserved `trace_id` with a fresh child `span_id != incoming_span_id` matching across handler, outbox/job capture, and response header (keeping `:517` standalone).
  - `RC-08` removes unused `Counter`/`Gauge`, so `counter_and_gauge_primitives` in `crates/core/tests/integration/metrics.rs:6-22` is deleted.
