# Plan Review — `002-modular-core-and-otel`

**Verdict:** `APPROVE` (updated on re-review; initial review was `REVISE`)

## Re-review Status

All **2 Critical (`C-1`, `C-2`)**, **5 Major (`M-1`–`M-5`)**, and **3 Minor (`m-1`–`m-3`)** findings from the initial review have been resolved in the updated task files (`01`–`06`):

- `C-1` resolved (`01`:19, `02`:13): `TraceContext`, `TRACEPARENT`, and `TRACESTATE` moved to `rume-observability/src/trace_context.rs`.
- `C-2` resolved (`02`:15,22,27, `03`:12,24, `06`:21): `MetricsRegistry::render_prometheus(&self) -> String` preserved; `register_pool` moved to `MetricsRegistryPoolExt` in `rume-data` and re-exported in `rume::prelude`.
- `M-1` resolved (`02`:22,28,34): `.without_scope_info().without_target_info()`, `HISTOGRAM_BUCKETS` `View`, and initial `http_requests_in_flight 0` handling specified.
- `M-2` resolved (`03`:12,18,23,25): `OutboxRepository` / `TransactionalOutbox` impls and non-DDL `outbox_*.sql` queries moved to `rume-messaging`; `outbox_schema.sql` retained in `rume-data`.
- `M-3` resolved (`01`:30,36, `06`:9-19): `schemars`, `redis` (4 crates), and `rume-testing?/{postgres,sqlite}` weak-dep feature forwarding wired.
- `M-4` resolved (`04`:20, `05`:18): `#[cfg(test)]` `TestApp` unit tests in `routing/group.rs`, `validation.rs`, and `app/builder.rs` rewritten/relocated.
- `M-5` resolved (`06`:22): `bin/crash_harness.rs` `include_str!` paths updated/relocated.
- `m-1`, `m-2`, `m-3` resolved (`01`:18,30,37,43, `04`:25, `05`:18, `06`:24).

---

## Initial Review Summary (Historical)

The 9-crate workspace DAG (`rume-foundation` → `rume-observability` → `rume-data` → `rume-messaging` / `rume-security` → `rume-http` → `rume-enterprise` / `rume-app` → `rume-testing` → `rume`) is architecturally sound, and the pinned OpenTelemetry versions (`opentelemetry = "=0.33.1"`, `opentelemetry_sdk = "=0.33.1"`, `opentelemetry-otlp = "=0.33.1"`, `opentelemetry-prometheus = "=0.33.0"`, `opentelemetry-appender-tracing = "=0.33.1"`, `tracing-opentelemetry = "=0.34.0"`) exist on crates.io and are mutually compatible.

However, **2 Critical** and **5 Major** gaps against the real codebase will cause compilation or test failures during scaffolding/execution if not resolved in the plan first: Rust coherence/orphan-impl conflicts on `TraceContext::current()` and `MetricsRegistry::register_pool()`, `MetricsRegistry::render_prometheus()` renaming, `opentelemetry-prometheus` `0.33.0` exposition defaults vs. `metrics.rs` assertions, `OutboxRepository` impls still inside `persistence/`, missing feature-forwarding (`schemars`, `redis`, `rume-testing?/{postgres,sqlite}`), `#[cfg(test)]` `crate::testing::TestApp` usages in `routing`/`validation`/`app`, and broken `include_str!` paths in `bin/crash_harness.rs`.

---

## Findings

### Critical (C)

1. **`[C-1]` Coherence conflict on `TraceContext::current()` between `rume-foundation` (Task 01) and `rume-observability` (Task 02)**
   - **Plan:** `01-workspace-pins-and-foundation.md:19,36` places `TraceContext` in `rume-foundation/src/telemetry.rs` with zero `rume-*` dependencies; `02-observability-crate-and-otel-backbone.md:20` states `TraceContext::current()` reads from `tracing::Span::current().context()` via `TraceContextPropagator` / `OpenTelemetrySpanExt` in `rume-observability`.
   - **Codebase:** `TraceContext::current()` is called in `crates/core/src/events/bus.rs:141`, `outbox/message.rs:73,102`, `jobs/job.rs:74`, `jobs/queue.rs:15,27`, `jobs/postgres.rs:106`, `testing/jobs.rs:104,121`, and `tests/integration/request_identity.rs:393,422`.
   - **Issue:** Rust forbids adding an inherent `impl TraceContext { pub fn current() ... }` in `rume-observability` if `TraceContext` is defined in `rume-foundation`.
   - **Fix:** Move `TraceContext`, `TRACEPARENT`, and `TRACESTATE` to `rume-observability` (nothing in `rume-foundation` uses `TraceContext`), or replace `TraceContext::current()` with an extension trait / free function `rume_observability::current_trace_context()`.

2. **`[C-2]` Coherence conflict on `MetricsRegistry::register_pool()` and method rename (`render_prometheus` → `encode_text`)**
   - **Plan:** `02-observability-crate-and-otel-backbone.md:14,21` moves `MetricsRegistry` to `rume-observability` (which does not depend on `rume-data`) and refers to `MetricsRegistry::encode_text()`.
   - **Codebase:** `crates/core/src/metrics/registry.rs:349-357` defines `pub fn register_pool<B: crate::persistence::DatabaseBackend + 'static>(&self, name: impl Into<String>, pool: &crate::persistence::DatabasePool<B>)` as an inherent method on `MetricsRegistry`. Additionally, `crates/core/src/metrics/registry.rs:423`, `routing/router.rs:412`, `app/builder.rs:782`, and `crates/core/tests/containers/pipeline.rs:238` all call `render_prometheus()`, not `encode_text()`.
   - **Issue:** `rume-data` cannot add an inherent `register_pool` method to `rume_observability::MetricsRegistry`, and renaming `render_prometheus()` to `encode_text()` breaks `tests/containers/pipeline.rs:238`.
   - **Fix:** Keep `MetricsRegistry::render_prometheus(&self) -> String`, and move `register_pool` to a `MetricsRegistryPoolExt` extension trait in `rume-data` (re-exported in `rume::prelude`) or a method on `DatabasePool`.

---

### Major (M)

3. **`[M-1]` `opentelemetry-prometheus` `0.33.0` exposition defaults vs. `metrics.rs` assertions**
   - **Plan:** `02-observability-crate-and-otel-backbone.md:21,26-27` assumes `SdkMeterProvider` + `opentelemetry-prometheus` `0.33.0` satisfies `crates/core/tests/integration/metrics.rs` and `registry.rs` unit tests.
   - **Codebase:** `crates/core/tests/integration/metrics.rs:43,64-67` asserts exact lines `http_requests_in_flight 0` (before any request is made), `http_requests_total{method="GET",status="200"} 2`, and `http_request_duration_seconds_count{method="GET"} 2`. `crates/core/src/metrics/registry.rs:461-563` asserts exact label/metric lines for `db_pool_connections`, `health_check_status`, `health_check_duration_seconds`, etc.
   - **Issue:** By default `opentelemetry-prometheus` emits `otel_scope_name`/`otel_scope_version` and `target_info` labels unless `.without_scope_info()` and `.without_target_info()` are set, may normalize counter/unit suffixes, and an OTel `UpDownCounter` emits no `http_requests_in_flight 0` series before its first `.add(0, &[])` observation.
   - **Fix:** Specify `.without_scope_info().without_target_info()`, initial zero-observation / `ObservableGauge` handling for `http_requests_in_flight`, and `SdkMeterProvider` `View` bucket configuration for `HISTOGRAM_BUCKETS` in Task 02.

4. **`[M-2]` `OutboxRepository` / `TransactionalOutbox` impls and `outbox_*.sql` queries still live in `persistence/`**
   - **Plan:** `03-data-and-messaging-crates.md:12,18,23-25` only mentions removing `crate::outbox::*` imports from `persistence` and adding `OutboxTransactionExt`.
   - **Codebase:** `crates/core/src/persistence/{pool.rs, sqlite.rs, postgres.rs, mysql.rs}` implement `OutboxRepository` and `TransactionalOutbox` for `DatabasePool<B>` and embed 15+ `sql/{sqlite,postgres,mysql}/outbox_*.sql` files.
   - **Fix:** Explicitly state in Task 03 that `impl OutboxRepository` and `impl TransactionalOutbox` for `DatabasePool<...>` (permitted by Rust's orphan rule since the traits are local to `rume-messaging`) and the non-DDL `outbox_*.sql` queries move from `rume-data` to `rume-messaging`, while `outbox_schema.sql` stays in `rume-data` for `Migrator::with_rume_core()`.

5. **`[M-3]` Incomplete feature-forwarding matrix (`schemars`, `redis`, `rume-testing?/{postgres,sqlite}`)**
   - **Plan:** `01-workspace-pins-and-foundation.md` omits `schemars`; `06-umbrella-reexports-cli-and-workbench.md:9` omits `schemars` and does not specify weak-dep forwarding for `rume-testing`.
   - **Codebase:**
     - `crates/core/Cargo.toml:32` and `crates/core/src/config.rs:263` gate schema generation behind `#[cfg(feature = "schemars")]`.
     - `#[cfg(feature = "redis")]` appears in `cache/`, `coordination/` (`rume-data`), `jobs/` (`rume-messaging`), `auth/`, `tenant/` (`rume-security`), and `resilience/` (`rume-http`).
     - `#[cfg(feature = "postgres")]` and `#[cfg(feature = "sqlite")]` appear in `testing/{mod.rs, harness.rs, fixtures.rs}` (`rume-testing`), while `crates/cli/Cargo.toml:14` and `workbench/multi-instance/Cargo.toml:7` enable `postgres`/`sqlite` on `rume`.
   - **Fix:** Add optional `schemars` to `rume-foundation` and `schemars = ["rume-foundation/schemars"]` to `rume`; forward `redis` to `rume-data/redis`, `rume-messaging/redis`, `rume-security/redis`, and `rume-http/redis`; use weak dependency syntax `"rume-testing?/postgres"` and `"rume-testing?/sqlite"` in `crates/core/Cargo.toml`.

6. **`[M-4]` `#[cfg(test)]` unit tests in `routing`, `validation`, and `app` import `crate::testing::TestApp`**
   - **Codebase:** `crates/core/src/routing/group.rs:220`, `crates/core/src/validation.rs:323`, and `crates/core/src/app/builder.rs:1873,1904,1935` use `crate::testing::TestApp` in `#[cfg(test)]` blocks.
   - **Plan:** Tasks 04 (`rume-http`) and 05 (`rume-app`) prohibit `rume-testing` as a regular dependency, but do not address these unit tests.
   - **Fix:** Rewrite these unit tests to use `tower::ServiceExt::oneshot` directly in `rume-http`/`rume-app` or relocate them to `rume-testing` / `crates/core/tests/`.

7. **`[M-5]` `crates/core/src/bin/crash_harness.rs` `include_str!` paths will break**
   - **Codebase:** `crates/core/src/bin/crash_harness.rs:22-26` embeds `include_str!("../persistence/sql/postgres/crash_side_effects_table.sql")` (and two sibling SQL files).
   - **Plan:** Task 06 removes `crates/core/src/persistence/` while keeping `bin/crash_harness.rs` in `crates/core/src/`.
   - **Fix:** Update the `include_str!` paths to `../../../data/src/persistence/sql/postgres/...` or relocate those three crash-harness SQL files into `crates/core/src/bin/`.

---

### Minor (m)

8. **`[m-1]` Preserve `rume::app::observability::*` and `rume::app::{Environment, ProjectPaths, ...}` module paths**
   - `workbench/multi-instance/src/routes.rs:2` uses `rume::app::observability::ObservabilityLayer`, and `crates/cli/templates/` uses `rume::app::{AppContext, Rume, init_tracing}`. Ensure `rume_app::app` (or `rume::app`) re-exports `pub mod observability` from `rume-observability` and `state`/`paths`/`env` from `rume-foundation`.

9. **`[m-2]` `HealthStatus` variant naming in plan text**
   - `01-workspace-pins-and-foundation.md:44` and `04-security-http-and-enterprise-crates.md:25` refer to `Healthy`/`Degraded`/`Unhealthy`, whereas `crates/core/src/routing/health.rs:25-32` defines `HealthStatus::{Ok, Warning, Failed}`. Keep the existing variant names.

10. **`[m-3]` `rume-foundation` requires `axum` due to the orphan rule**
    - Because `AppError` (`impl IntoResponse`) and `RequestId` (`impl FromRequestParts`) move to `rume-foundation` (`01-workspace-pins-and-foundation.md:15,19`), `rume-foundation` must depend on `axum`. Document this explicitly in Task 01's deliverables.
