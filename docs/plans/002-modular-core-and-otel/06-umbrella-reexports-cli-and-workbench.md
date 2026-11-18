# Task 06 — Umbrella `rume` Crate, `rume-cli`, `workbench/*`, & Whole-Workspace Verification

## Goal

Convert `crates/core` into the clean umbrella `rume` crate re-exporting all layered crates, `prelude`, and `rume-macros`; wire `feature = "otel"` (gating `opentelemetry-otlp` while keeping W3C propagation + `tracing-opentelemetry` on by default); remove all migrated source files from `crates/core/src/` (keeping `bin/crash_harness.rs`); and verify `rume`, `rume-cli`, `xtask`, and `workbench/*` compile, pass lints, and pass tests.

## Files touched

- `crates/core/Cargo.toml` (depend on `rume-foundation`, `rume-observability`, `rume-data`, `rume-messaging`, `rume-security`, `rume-http`, `rume-enterprise`, `rume-app`, `rume-macros`, and optional `rume-testing`; wire full feature-forwarding matrix:
  - `default = ["sqlite", "testing"]`
  - `otel = ["rume-observability/otel"]`
  - `testing = ["dep:rume-testing"]`
  - `schemars = ["rume-foundation/schemars"]`
  - `redis = ["rume-data/redis", "rume-messaging/redis", "rume-security/redis", "rume-http/redis"]`
  - `postgres = ["rume-data/postgres", "rume-messaging/postgres", "rume-testing?/postgres"]`
  - `sqlite = ["rume-data/sqlite", "rume-messaging/sqlite", "rume-testing?/sqlite"]`
  - `mysql = ["rume-data/mysql", "rume-messaging/mysql"]`
  - `saml = ["rume-enterprise/saml"]`
  - `enterprise = ["rume-enterprise/enterprise"]`)
- `crates/core/src/lib.rs` (re-export `app`, `auth`, `cache`, `config`, `coordination`, `enterprise`, `env`, `error`, `events`, `helpers`, `jobs`, `metrics`, `module`, `openapi`, `outbox`, `persistence`, `resilience`, `routing`, `saga`, `security`, `tenant`, `testing`, `validation`, top-level `pub use`, and `rume_macros::{embed_migrations, sql_assets}`)
- `crates/core/src/prelude.rs` (preserve all existing exports + `rume_data::MetricsRegistryPoolExt` + `rume_testing::RumeTestingExt` under `#[cfg(feature = "testing")]`)
- `crates/core/src/bin/crash_harness.rs` (update `include_str!` paths to `../../../data/src/persistence/sql/postgres/crash_side_effects_*.sql` or relocate the three crash-harness SQL files into `crates/core/src/bin/`)
- `crates/core/tests/integration/*.rs` and `crates/core/tests/containers/*.rs` (update only if any test referenced `CURRENT_TRACE_CONTEXT` directly, replacing with `TraceContext::current()` / OTel `tracing_opentelemetry::OpenTelemetrySpanExt`)
- `crates/cli/` and `workbench/*/` (verify `rume doctor`, `rume::app::observability::*`, `rume::app::{Environment, ProjectPaths, init_tracing}`, and `Cargo.toml` feature compatibility; update only if needed)

## Deliverable

1. `crates/core` (`rume`) is a pure umbrella facade + `bin/crash_harness.rs` + integration/container test suites.
2. `use rume::prelude::*;` and `rume::<module>::*` compile unchanged for `crates/core/tests/*`, `crates/cli`, and all three `workbench/*` apps.
3. `cargo tree -p rume --edges normal` confirms a strict acyclic DAG across all `rume-*` crates with zero cycles.
4. `cargo check -p rume --features otel -j 2` succeeds, enabling OTLP export.

## Constraints honoured

- `crates/core/src/lib.rs`: `#![forbid(unsafe_code)]`, stability tier doc comments, and all public module names preserved.
- `crates/core/src/prelude.rs`: Every symbol listed in `crates/core/src/prelude.rs` lines 3–21 remains exported.
- `crates/core/tests/integration/request_identity.rs`: HTTP -> Outbox -> Job W3C `traceparent` propagation passes using OTel `TraceContextPropagator` + `OpenTelemetrySpanExt` (no `CURRENT_TRACE_CONTEXT`).
- `crates/core/tests/integration/architecture.rs`, `metrics.rs`, `persistence.rs`, `test_server.rs`, `property_tests.rs`.
- `crates/cli/tests/generated_project.rs`, `crates/cli/tests/scaffold_template.rs`.
- `rules/40-verification.md`: Whole-workspace format, clippy (zero warnings), build, and test verification.

## Named tests (bodies omitted)

- `umbrella_prelude_and_module_reexports_compile` — verified by compiling `crates/core/tests/integration.rs`, `crates/cli`, and `workbench/*` without changing their `use rume::prelude::*;` imports.
- `integration_request_identity_propagates_otel_trace_across_http_outbox_job` — `crates/core/tests/integration/request_identity.rs` passes end-to-end with OTel span bridging.
- `integration_metrics_exposes_otel_prometheus_text_format` — `crates/core/tests/integration/metrics.rs` passes end-to-end against `SdkMeterProvider` + `opentelemetry-prometheus`.

## Verification

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -j 2 -- -D warnings
cargo check -p rume --features otel -j 2
cargo test -p rume --test integration -j 2
cargo test -p rume-cli -j 2
cargo check -p workbench-simple-api -p workbench-modular-monolith -p workbench -j 2
```

Expected: Zero formatting diffs, zero clippy warnings across the workspace, `otel` feature builds cleanly, all `rume` integration tests and `rume-cli` tests pass, and all `workbench/*` crates check cleanly.

## Commit

Only when the user asks:

```text
refactor(core): convert crates/core to umbrella rume crate over layered workspace DAG
```

## Done-when

- [ ] `crates/core/src/` contains only `lib.rs`, `prelude.rs`, and `bin/crash_harness.rs`.
- [ ] `cargo fmt --all --check` exits 0.
- [ ] `cargo clippy --workspace --all-targets -j 2 -- -D warnings` exits 0.
- [ ] `cargo check -p rume --features otel -j 2` exits 0.
- [ ] `cargo test -p rume --test integration -j 2` passes.
- [ ] `cargo test -p rume-cli -j 2` passes.
- [ ] `cargo check -p workbench-simple-api -p workbench-modular-monolith -p workbench -j 2` exits 0.
