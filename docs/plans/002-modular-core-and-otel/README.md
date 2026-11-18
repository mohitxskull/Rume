# Plan 002 — Modular Core Workspace Split & OpenTelemetry Backbone

## Goal

Refactor `rume` (`crates/core`) before `v0.1.0` from a single cyclic crate into a clean, layered workspace DAG under an umbrella `rume` crate, with OpenTelemetry as the unified observability backbone:

1. **Break the 12-module internal dependency cycle** in `crates/core/src/` (`{app, cache, error, events, jobs, metrics, module, openapi, outbox, persistence, routing, testing}`) by extracting `rume-foundation` and inverting back-edges (`app <-> testing`, `metrics <-> persistence/routing`, `outbox <-> persistence`, `openapi <-> routing`, `app <-> module`, `app <-> routing`).
2. **Split `crates/core` into layered workspace crates** (`rume-foundation`, `rume-observability`, `rume-data`, `rume-messaging`, `rume-security`, `rume-http`, `rume-enterprise`, `rume-app`, `rume-testing`) re-exported through the umbrella `rume` crate (`crates/core`), preserving `use rume::prelude::*;` and `rume::<module>` paths.
3. **Integrate OpenTelemetry (OTel) as the unified observability backbone**:
   - Keep `tracing` as the instrumentation facade (Axum/Tower/SQLx/Tokio).
   - Replace `CURRENT_TRACE_CONTEXT` (`tokio::task_local!`) in `crates/core/src/app/observability.rs` with W3C `TraceContextPropagator` + `tracing_opentelemetry::OpenTelemetrySpanExt` bridging across HTTP -> Outbox -> Jobs -> Saga.
   - Enable core W3C propagation + `tracing-opentelemetry` span bridging by default, and gate `opentelemetry-otlp` behind `feature = "otel"`.
   - Migrate `MetricsRegistry` (`crates/core/src/metrics/registry.rs`) to `opentelemetry::metrics` (`SdkMeterProvider`) + `opentelemetry-prometheus` for `/metrics` scraping, unifying traces, logs, and metrics under OTel.

## Current state (read before executing)

- `crates/core/src/` currently houses all 22 modules in a single crate (`rume`) with `#![forbid(unsafe_code)]` in `crates/core/src/lib.rs`.
- `crates/core/Cargo.toml` defines features `default = ["sqlite", "testing"]`, `testing`, `postgres`, `redis`, `sqlite`, `mysql`, `saml`, `enterprise`.
- `crates/core/src/app/observability.rs` implements `TraceContext`, `ObservabilityLayer`, `init_tracing`, and `try_init_tracing` using `CURRENT_TRACE_CONTEXT: tokio::task_local!`.
- `crates/core/src/metrics/registry.rs` implements `MetricsRegistry` backed directly by `prometheus = "=0.14.0"` and imports `crate::persistence::PoolStatus` and `crate::routing::HealthStatus`.
- `crates/core/src/persistence/{pool.rs, postgres.rs, sqlite.rs, mysql.rs}` imports `crate::outbox::*`, `crate::metrics::MetricsRegistry`, and `crate::routing::{HealthCheck, HealthCheckResult, HealthStatus}`.
- `crates/core/src/app/builder.rs` imports `crate::testing::TestApp`, `crate::module::*`, `crate::openapi::*`, and `crate::routing::*`.
- Consumers in this workspace: `crates/cli/Cargo.toml`, `crates/macro/Cargo.toml`, `crates/xtask/Cargo.toml`, `workbench/simple-api/Cargo.toml`, `workbench/modular-monolith/Cargo.toml`, `workbench/multi-instance/Cargo.toml`.

## Global constraints

- **Concrete dependency pins (`rules/10-tech-stack.md`):** Every new dependency in root `Cargo.toml` `[workspace.dependencies]` MUST use an exact `=x.y.z` pin.
- **No unsafe code (`docs/specs/vision/ROADMAP.md` `REL-12`):** Every new workspace crate MUST declare `#![forbid(unsafe_code)]` in its `src/lib.rs`.
- **CPU-constrained verification:** Scope `cargo check`, `cargo clippy`, and `cargo test` commands with `-j 2` to avoid overloading the local machine.
- **No commits without explicit user request (`rules/30-git.md`):** Follow Conventional Commits only when authorized.
- **Out of scope:** Changing HTTP routing semantics, SQL behavior, or SAML/SCIM business logic beyond moving modules and wiring OTel propagation; Kani/Verus verification harnesses.

## Constraints honoured

- `crates/core/src/lib.rs` (`#![forbid(unsafe_code)]`, stability tier docs, `rume_macros::{embed_migrations, sql_assets}` re-exports).
- `crates/core/src/prelude.rs` (`use rume::prelude::*;` public surface).
- `crates/core/tests/integration/request_identity.rs` (W3C `traceparent`/`tracestate` HTTP -> Outbox -> Job propagation).
- `crates/core/tests/integration/property_tests.rs` (`TraceContext` invariants and roundtripping).
- `crates/core/tests/integration/metrics.rs` (`/metrics` Prometheus text exposition format and `MetricsRegistry` behavior).
- `crates/core/tests/integration/test_server.rs`, `crates/core/tests/integration/architecture.rs`, `crates/core/tests/integration/persistence.rs`.
- `crates/cli/tests/generated_project.rs`, `crates/cli/tests/scaffold_template.rs`.
- `workbench/simple-api`, `workbench/modular-monolith`, `workbench/multi-instance`.

## Task index

- `01-workspace-pins-and-foundation.md` — Pin OTel 0.33.x / `tracing-opentelemetry` 0.34.0 in `Cargo.toml` and extract `rume-foundation` (`crates/foundation`).
- `02-observability-crate-and-otel-backbone.md` — Create `rume-observability` (`crates/observability`) with unified OTel traces/logs/metrics, `opentelemetry-prometheus`-backed `MetricsRegistry`, and `ObservabilityLayer` replacing `CURRENT_TRACE_CONTEXT`.
- `03-data-and-messaging-crates.md` — Create `rume-data` (`crates/data`) and `rume-messaging` (`crates/messaging`), inverting `outbox <-> persistence` and wiring OTel context propagation across events, outbox, and jobs.
- `04-security-http-and-enterprise-crates.md` — Create `rume-security` (`crates/security`), `rume-http` (`crates/http`), and `rume-enterprise` (`crates/enterprise`) with OTel span propagation across saga steps.
- `05-app-and-testing-crates.md` — Create `rume-app` (`crates/app`) and `rume-testing` (`crates/testing`), inverting `app <-> testing`, `app <-> module`, and `module <-> testing`.
- `06-umbrella-reexports-cli-and-workbench.md` — Convert `crates/core` to the umbrella `rume` crate with `otel` feature gating, verify `crates/cli`, `crates/xtask`, `workbench/*`, and run whole-workspace verification.

## Done-when checklist

- [ ] Root `Cargo.toml` pins all OTel crates with exact `=x.y.z` versions and lists all new `crates/*` members.
- [ ] Every new crate under `crates/` has `#![forbid(unsafe_code)]` and zero circular crate dependencies (`cargo tree --workspace` forms a strict DAG).
- [ ] `CURRENT_TRACE_CONTEXT` `tokio::task_local!` is removed; W3C `TraceContext` propagation across HTTP -> Outbox -> Jobs -> Saga uses `opentelemetry` `TraceContextPropagator` + `tracing_opentelemetry::OpenTelemetrySpanExt`.
- [ ] `MetricsRegistry` is backed by `opentelemetry::metrics::SdkMeterProvider` + `opentelemetry-prometheus` and continues to serve valid Prometheus text format on `/metrics`.
- [ ] `opentelemetry-otlp` is gated behind `feature = "otel"` on `rume`, while default features include W3C propagation and `tracing-opentelemetry` span bridging.
- [ ] `use rume::prelude::*;` and `rume::<module>` paths remain intact for `crates/core/tests/*`, `crates/cli`, and `workbench/*`.
- [ ] `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -j 2 -- -D warnings`, and `cargo test -p rume --test integration -j 2` pass.

## Verification commands

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -j 2 -- -D warnings
cargo test -p rume --test integration -j 2
cargo test -p rume-cli -j 2
cargo check -p workbench-simple-api -p workbench-modular-monolith -p workbench -j 2
```
