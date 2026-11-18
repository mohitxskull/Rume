# Task 01 — Workspace OTel Pins & `rume-foundation`

## Goal

Add concrete `=x.y.z` OpenTelemetry dependency pins to `Cargo.toml` and extract `rume-foundation` (`crates/foundation`) with zero internal workspace dependencies, breaking the `error <-> app`, `cache -> routing`, `metrics -> routing`, `persistence -> routing`, and `module -> app` edges.

## Files touched

- `Cargo.toml` (`[workspace.members]`, `[workspace.dependencies]`)
- `crates/foundation/Cargo.toml`
- `crates/foundation/src/lib.rs`
- `crates/foundation/src/env/` (moved from `crates/core/src/env/`)
- `crates/foundation/src/config.rs` (moved from `crates/core/src/config.rs`)
- `crates/foundation/src/helpers/` (moved from `crates/core/src/helpers/`)
- `crates/foundation/src/error.rs` (moved from `crates/core/src/error.rs`)
- `crates/foundation/src/state.rs` (moved from `crates/core/src/app/state.rs`: `Lifecycle`, `LifecycleState`, `Availability`, `AvailabilityState`, `ApplicationMode`)
- `crates/foundation/src/paths.rs` (moved from `crates/core/src/app/paths.rs`: `ProjectPaths`)
- `crates/foundation/src/health.rs` (health contract types from `crates/core/src/routing/health.rs`: `HealthStatus` (`Ok`, `Warning`, `Failed`), `HealthCheckResult`, `HealthCheck`, `HealthChecks`, `HealthReport`)
- `crates/foundation/src/telemetry.rs` (`PoolStatus` snapshot struct from `crates/core/src/persistence/pool.rs`, `RequestId`, `X_REQUEST_ID`)

## Deliverable

1. Root `Cargo.toml` pins:
   - `opentelemetry = "=0.33.1"`
   - `opentelemetry_sdk = "=0.33.1"`
   - `opentelemetry-otlp = "=0.33.1"`
   - `opentelemetry-prometheus = "=0.33.0"`
   - `opentelemetry-appender-tracing = "=0.33.1"`
   - `tracing-opentelemetry = "=0.34.0"`
2. `crates/foundation` (`rume-foundation`) compiles standalone with `#![forbid(unsafe_code)]`, zero dependencies on any other `rume-*` crate, depends on `axum` (required by Rust's orphan rule for `AppError: IntoResponse` and `RequestId: FromRequestParts`), and exposes optional `schemars` (`schemars = "=1.2.3"`) for `config.rs`. All unit tests in `env`, `config`, `helpers`, `error`, `state`, `paths`, `health`, and `telemetry` pass.

## Constraints honoured

- `rules/10-tech-stack.md`: All new dependencies in `Cargo.toml` use exact `=x.y.z` pins.
- `crates/core/src/error.rs`: `AppError` RFC 9457 Problem Details serialization, `InternalSource` span/backtrace capture, and `AppError::into_response_in_env(&Environment)` preserved without importing `app`.
- `crates/core/Cargo.toml` (`schemars` feature) and `crates/core/src/config.rs:263`: `#[cfg(feature = "schemars")]` JSON Schema generation preserved.
- `crates/core/src/routing/health.rs`: `HealthChecks` and `HealthStatus` (`Ok`, `Warning`, `Failed`) preserved without pulling Axum routing handlers into `rume-foundation`.
- `crates/core/src/helpers/`: `safe_equal` (`subtle`) preserved.

## Named tests (bodies omitted)

- `foundation_error_renders_rfc9457_problem_details_and_redacts_internal_in_prod` — asserts `AppError::Internal` hides diagnostic details in production `Environment` and exposes them in development.
- `foundation_health_checks_aggregates_worst_status` — asserts `HealthChecks::run_all` returns `HealthStatus::Failed` when any check fails and `HealthStatus::Warning` when warned without failures.

## Verification

```bash
cargo check -p rume-foundation -j 2
cargo test -p rume-foundation -j 2
```

Expected: `rume-foundation` compiles with 0 warnings and all unit tests pass.

## Commit

Only when the user asks:

```text
refactor(foundation): extract rume-foundation crate and pin OTel 0.33.x deps
```

## Done-when

- [ ] `Cargo.toml` has exact `=x.y.z` pins for all 6 OTel/tracing-otel crates.
- [ ] `crates/foundation/src/lib.rs` has `#![forbid(unsafe_code)]` and zero `rume-*` dependencies in `crates/foundation/Cargo.toml`.
- [ ] `cargo test -p rume-foundation -j 2` passes with 0 warnings.
