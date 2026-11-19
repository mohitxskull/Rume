# Plan ledger — plan: docs/plans/003-audit-remediation-and-hardening/

Ruling: Execute Plan 003 on branch `fix/audit-remediation-003` (base `main` `31683f6`) in 3 parallel disjoint waves, reusing persistent implementer subagents per user instruction.
Ruling: Skip all review subagents per explicit user instruction ("dont use review subagents no need to do review").
Ruling: Commit locally per wave/milestone on `fix/audit-remediation-003`, then push all commits and open a new PR targeting `main`.
Ruling: Upgrade Tasks 01, 02, 03, and 05 to use `tokio::task::JoinSet`, direct MySQL session execution, `sqlparser = "=0.53.0"`, and `chrono = "=0.4.45"`.
Ruling: Enforce `rules/70-code-rationale.md` — code + explicit `Why:` comments stand alone as single source of truth.
Ruling: Keep all Cargo check/clippy/test commands scoped with `-j 2` per local CPU constraint.

## Pre-flight scan

| Scope / Wave | Tasks | Disjoint Ownership | Notes |
|---|---|---|---|
| Step 0 | Prep | `.coderabbit.yaml`, Cargo pins, `docs/plans/003-...`, `#![forbid(unsafe_code)]` in `macro`/`xtask` | Clean base before Wave 1 |
| Wave 1 (parallel) | `01`, `02`, `04`, `07` | `typed.rs`+`worker.rs` vs `migration.rs`+`core_down.sql` vs `outbox/*`+`jobs/postgres.rs` vs `builder.rs`+`ext.rs` | Zero overlapping files; Task 02 updates integration/workbench migration len tests |
| Wave 2 (parallel) | `03`, `05`, `06` | `persistence/{row,backend,pg,my,sq}.rs` vs `foundation`+`saml`+`auth/redis`+`cli/db` vs `tenant/{cache,quota}.rs`+`typed.rs` (`default_ttl`) | `cache/typed.rs` is disjoint across Wave 1 -> Wave 2; note `core/src/lib.rs` only touched by Task 05 (deleting `safe_equal_hashed`) |
| Wave 3a (parallel) | `08`, `09` | `observability/*`+`events/bus.rs`+`saga/coordinator.rs`+`core/src/lib.rs` vs `crates/crash-harness`+SQL boundary moves | Zero overlapping files |
| Wave 3b | `10` | `testing/fixtures.rs` + `proptest` suites + `rume-cli` templates/CLI `--dialect` | Final task after all workspace refactoring lands |

## Wave 1
Task 1 (`RC-01`, Worker C): complete — RAII `SingleflightLeaderGuard` + follower re-contention loop in `Cache::remember_with_tags`; `tokio::task::JoinSet` in `JobWorker::run_loop`.
Task 2 (`RC-02`/`OQ-01`, Worker A): complete — immutable `20261104_001_rume_core_schemas`, preserved `rume_migrations` across `core_down.sql`, append-only `20261104_002_rume_audit_schema`, and direct `PoolConnection<MySql>` session execution under `MysqlMigrationLockGuard`.
Task 4 (`RC-03`, Worker B): complete — SQLite `BEGIN IMMEDIATE` + `fetch_optional` outbox claim, non-locking `outbox_fetch_pending.sql` for Postgres/MySQL, strict `trace_context` selection/decoding in Postgres DLQ listings.
Task 7 (`RC-06`, Worker D): complete — `crates/app/src/app/lifecycle.rs` shared coordinator, finalized `AppContext` propagation, removed dead `RumeBuilder::state`, `ApplicationMode::Inspect` DAG + route checks.
Wave 1 verified clean with `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -j 2 -- -D warnings`.

## Wave 2
Task 3 (`RC-09`, Worker A): complete — `FromDbValue` emits `PersistenceError::DecodeError`, `From<usize> for DbValue` delegates to `u64`, `validate_managed_transaction_sql` & `reject_unbound_sqlite_placeholders` rewritten with `sqlparser = "=0.53.0"`, and bind loops deduplicated in `mysql.rs`/`sqlite.rs`.
Task 5 (`RC-04`/`OQ-02`, Worker C): complete — fail-closed `fs::canonicalize` in `loading.rs`/`db/mod.rs`, `SamlParser` hardened with `chrono = "=0.4.45"` RFC 3339 parsing, `XmlNs` `<Issuer>` anchoring, `signed_mode` `Assertion/@ID` reference enforcement, Redis `REGENERATE_ID_SCRIPT` migrated to guarded `cjson` (with `#[serde(default)]` on optional `Session` fields), and unused `safe_equal_hashed` deleted.
Task 6 (`RC-07`, Worker B): complete — `Cache::default_ttl` / `resolve_ttl` propagated before `quota.record_key`, rollback guard handles backend/closed cache errors, `RedisTenantQuotaStore::key()` double-prefix fixed, and rolling `PEXPIRE`/`PERSIST` added.
Wave 2 verified clean with `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -j 2 -- -D warnings`.

## Wave 3a
Task 8 (`RC-08`, Worker C): complete — `ObservabilityLayer` extracts incoming `traceparent` without ambient `OtelContext::current()` leaks and emits server child `TraceContext`, `EventBus::publish_sync` creates `event.publish_sync` span + `Why:` notes in `bus.rs`/`coordinator.rs`, `MetricsRegistry` migrated from `PeriodicReader` + `InMemoryMetricExporter` to `ManualReader` (`experimental_metrics_custom_reader`), and unused `Counter`/`Gauge` deleted.
Task 9 (`RC-05`, Worker B): complete — eliminated all 22 cross-crate `include_str!("../../../data/...")` paths, externalized `PostgresJobQueue::defer` to `jobs_defer.sql`, moved `rume-crash-harness` + `crash_*.sql` into `publish = false` `crates/crash-harness`, demoted `tokio`/`sqlx`/`uuid` to `[dev-dependencies]` in `crates/core/Cargo.toml`, and verified all 12 publishable crates with `cargo package --list --allow-dirty` + 4/4 live Docker `crash_recovery` tests.
Wave 3a verified clean with `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -j 2 -- -D warnings`.
