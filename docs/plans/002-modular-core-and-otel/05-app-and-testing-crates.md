# Task 05 — `rume-app` & `rume-testing` (Inverting `app <-> testing` and `module <-> testing`)

## Goal

Create `rume-app` (`crates/app`: `app`, `module`) and `rume-testing` (`crates/testing`: `testing`, `ModuleTestHarness`, `RumeTestingExt`), breaking the `app <-> testing`, `app <-> module`, and `module <-> testing` cycles.

## Files touched

- `Cargo.toml` (`[workspace.members]`: add `"crates/app"`, `"crates/testing"`)
- `crates/app/Cargo.toml`, `crates/app/src/lib.rs`
- `crates/app/src/module/` (moved from `crates/core/src/module/`, excluding `harness.rs`)
- `crates/app/src/app/` (moved from `crates/core/src/app/`, excluding `state.rs`, `paths.rs`, and `observability.rs` which moved to `rume-foundation`/`rume-observability`, and removing `test_app` / `build_test_app` from `RumeBuilder`)
- `crates/testing/Cargo.toml`, `crates/testing/src/lib.rs`
- `crates/testing/src/` (moved from `crates/core/src/testing/` + `crates/core/src/module/harness.rs` + `RumeTestingExt` extension trait implementing `.test_app()` and `.build_test_app()` on `RumeBuilder`)

## Deliverable

1. `rume-app` depends on `rume-foundation`, `rume-observability`, and `rume-http`, with **zero** dependency on `rume-testing`. `rume_app::app` re-exports `pub mod observability` from `rume-observability` and `state`, `paths`, `Environment` from `rume-foundation` so `rume::app::observability::ObservabilityLayer` and `rume::app::{AppContext, Environment, ProjectPaths, Rume, init_tracing}` continue to resolve. Existing `#[cfg(test)]` unit tests in `app/builder.rs:1873,1904,1935` that used `crate::testing::TestApp` are rewritten to use `tower::ServiceExt::oneshot` directly (or relocated to `rume-testing` / `crates/core/tests/`).
2. `RumeBuilder` exposes the minimal `pub` hook(s) needed by `rume-testing`'s `RumeTestingExt` (e.g. `into_router_and_context()` / `build_with_modules()`) with a `///` doc note explaining it supports `rume-testing` (`rules/testing.md`).
3. `rume-testing` depends on `rume-foundation`, `rume-observability`, `rume-http`, `rume-data`, `rume-messaging`, and `rume-app`, providing `TestApp`, `TestAppBuilder`, `TestServer`, `ModuleTestHarness`, `TestJobs`, `TestOutbox`, `TestEvents`, `TestDb`, and `RumeTestingExt`.

## Constraints honoured

- `crates/core/src/app/builder.rs`: `Rume::new()`, `RumeBuilder::module()`, topological startup (`on_start`) and reverse-topological rollback/shutdown (`on_shutdown`), readiness-before-drain (`AvailabilityState::RefusingTraffic`), and `ObservabilityLayer` attachment preserved.
- `crates/core/src/module/`: Cycle detection (DFS) and missing-dependency preflight failure preserved.
- `crates/core/tests/integration/test_server.rs`: `TestServer` keeps application modules and pools alive until TCP connections drain.
- `crates/core/src/prelude.rs`: `RumeTestingExt` will be re-exported in `rume::prelude::*` in Task 06 so `Rume::new().test_app()` and `.build_test_app().await` continue to compile unchanged.

## Named tests (bodies omitted)

- `app_builder_topological_startup_and_reverse_shutdown_on_failure` — asserts module `on_start` runs in dependency order and failed start triggers reverse `on_shutdown` rollback.
- `testing_ext_builds_test_app_and_runs_teardown_hooks` — asserts `Rume::new().build_test_app().await` runs module lifecycle startup and executes teardown on drop/shutdown.

## Verification

```bash
cargo check -p rume-app -j 2
cargo test -p rume-app -j 2
cargo check -p rume-testing -j 2
cargo test -p rume-testing -j 2
```

Expected: `rume-app` and `rume-testing` compile with 0 warnings and unit tests pass.

## Commit

Only when the user asks:

```text
refactor(app,testing): extract rume-app and rume-testing, inverting testing back-edge
```

## Done-when

- [ ] `crates/app/Cargo.toml` has no regular dependency on `rume-testing`.
- [ ] `crates/app/src/lib.rs` and `crates/testing/src/lib.rs` declare `#![forbid(unsafe_code)]`.
- [ ] `RumeTestingExt` provides `.test_app()` and `.build_test_app()` on `RumeBuilder`.
- [ ] `cargo test -p rume-app -j 2` and `cargo test -p rume-testing -j 2` pass.
