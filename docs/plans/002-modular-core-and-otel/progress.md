# Plan ledger — plan: docs/plans/002-modular-core-and-otel/

Ruling: Skip per-task reviews per explicit user instruction; run one whole-plan review after Task 06 completes.
Ruling: Commit per task locally on current branch `fix/audit-findings-001` (PR #6, base `5bee7ce`) per explicit user instruction ("keep making commit in this same pr"); do not push until requested or run finishes.
Ruling: Keep all Cargo build/check/clippy/test commands scoped with `-j 2` per local CPU constraint.

## Pre-flight scan

| Scope / Pair | Status | Notes |
|---|---|---|
| Plan review (`review/plan-review.md`) | `APPROVE` | All 2 Critical, 5 Major, 3 Minor findings resolved and re-verified before execution. |
| `01` <-> `02` | Clean | `01` creates `rume-foundation` (`Environment`, `AppError`, `PoolStatus`, `HealthStatus`, `schemars` feature); `02` creates `rume-observability` (`TraceContext`, `MetricsRegistry` via `opentelemetry-prometheus`). |
| `02` <-> `03` | Clean | `03` creates `rume-data` + `rume-messaging` (`MetricsRegistryPoolExt` in `rume-data`, `OutboxRepository` impls + non-DDL `outbox_*.sql` in `rume-messaging`). |
| `03` <-> `04` | Clean | `04` creates `rume-security`, `rume-http`, `rume-enterprise` on top of `01`-`03`. |
| `04` <-> `05` | Clean | `05` creates `rume-app` and `rume-testing` (`RumeTestingExt`, `ModuleTestHarness`, `#[cfg(test)]` `TestApp` unit tests relocated/rewritten). |
| `05` <-> `06` | Clean | `06` converts `crates/core` to umbrella `rume` with full feature forwarding, `crash_harness.rs` SQL paths, and verifies `rume-cli`, `xtask`, `workbench/*`. |

Base HEAD: `5bee7ce`

Task 1: complete (5bee7ce..bed8649, per-task review skipped per user ruling)
Ruling: Added `HealthCheckObserver` trait + `HealthChecks::with_observer` in `rume-foundation::health` (implemented by `MetricsRegistry`) to invert `health <-> metrics` without `rume-foundation` depending on `rume-observability`.

Task 2: complete (bed8649..4e49253, per-task review skipped per user ruling)
Ruling: `opentelemetry-prometheus = "=0.33.0"` uses `.scope_info_enabled(false)` and appends `_total` to monotonic counters, so OTel counter names omit `_total` to preserve `http_requests_total`, `health_checks_total`, and `health_check_failures_total` exposition names.

Task 3: complete (4e49253..3b07e14, per-task review skipped per user ruling)

Task 4: complete (3b07e14..ffb0f96, per-task review skipped per user ruling)
Ruling: Relocated `health_check_cache_ttl_edge_cases_match_contract` into `crates/foundation/src/health.rs` because it inspects private `HealthChecks::cache` / `get_cached_report` internals owned by `rume-foundation`.
Ruling: Added `FromRef<AppContext<E, C, S>>` for `Availability`/`HealthChecks` and kept a thin `rume::routing::health_routes::<E, C, S>()` wrapper in `crates/core/src/routing.rs` so existing 3-type-parameter callers stay unchanged.


Task 5: complete (ffb0f96..345b4eb, per-task review skipped per user ruling)
Ruling: Kept `ModuleHarness` struct name in `rume-testing::module_harness` and added `pub type ModuleTestHarness<M> = ModuleHarness<M>;` (re-exporting both in `rume::module`) so existing `rume::module::ModuleHarness` integration test imports resolve unchanged.

Task 6: complete (345b4eb..5edd37e, per-task review skipped per user ruling)

Whole-plan review (`5bee7ce..5edd37e`): PASS (0 Critical, 0 Major, 3 Minor).
Minor fixes (`0703691..35ef99f`): resolved `[m-1]`, `[m-2]`, `[m-3]` (ephemeral `127.0.0.1:0` test port + `#[cfg(feature = "sqlite")]` test gating); `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -j 2 -- -D warnings`, `rume-app` (29+1), `rume-data` (4), and `rume-messaging` (13) tests pass with 0 warnings.

Follow-up (`d0f7c6c..7699c35`): removed `prometheus` and `opentelemetry-prometheus` completely per user request; migrated `MetricsRegistry` to pure `SdkMeterProvider` + `InMemoryMetricExporter` (`snapshot()` / `export_metrics()`) + optional OTLP `MetricExporter` under `feature = "otel"`, and removed Prometheus `.metrics()` route helpers. Verified fmt, clippy (`-D warnings`), `rume --features otel`, `rume-observability` (14), `rume-data` (6), `rume --test integration` (229), `rume-cli` (32+2+3), and `workbench/*`.
