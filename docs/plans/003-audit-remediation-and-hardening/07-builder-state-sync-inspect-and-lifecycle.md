# Task 07 — `RumeBuilder` State Sync, `Inspect` Mode Validation & Lifecycle Consolidation (`RC-06`)

## Goal

Ensure `TestAppParts.ctx` and live `on_ready` / `on_shutdown` hooks receive the finalized `route_registry` and `health_checks`, remove the dead `state: S` field from `RumeBuilder`, validate module DAGs and route collisions in `ApplicationMode::Inspect`, and deduplicate the startup/rollback/shutdown lifecycle runner between `rume-app` and `rume-testing`.

## Files touched

- `crates/app/src/app/lifecycle.rs` (new shared lifecycle coordinator module)
- `crates/app/src/app/mod.rs` (expose `pub mod lifecycle;`)
- `crates/app/src/app/builder.rs` (`RumeBuilder`, `build_router_with_ctx`, `into_test_parts`, `run_with_signal`, `run_with_listener_and_signal`, `run_inspect_mode`, `run_with_bound_listener`)
- `crates/testing/src/ext.rs` (`RumeTestingExt::build_test_app`)

## Deliverable

1. **Finalized context propagation (`crates/app/src/app/builder.rs`):**
   - Introduce internal `build_router_with_ctx(self, mut ctx: AppContext<E, C, S>) -> Result<(Router<AppContext<E, C, S>>, AppContext<E, C, S>), BuildError>` that mounts module routes/health checks, validates middleware/routes, binds `ctx` into the router, and returns the finalized `ctx` alongside the router; delegate `pub fn build_router` to `Ok(self.build_router_with_ctx(ctx)?.0)`.
   - Update `into_test_parts`, `run_with_signal`, and `run_with_listener_and_signal` to capture `(router, serve_ctx) = self.build_router_with_ctx(...)` so `TestAppParts.ctx`, `TestAppParts.router.with_state(serve_ctx.clone())`, `on_ready`, and `on_shutdown` observe all module routes and module health checks.
2. **Remove dead `state: S` field (`crates/app/src/app/builder.rs:282,330,334,698`):** Delete `state: S` from `RumeBuilder` (`self.ctx.state` is already authoritative).
3. **`ApplicationMode::Inspect` validation & deduplication:**
   - Extract a single `run_inspect_mode(mut self) -> Result<(), std::io::Error>` helper called by both `run_with_signal` and `run_with_listener_and_signal`.
   - Validate `ModuleRegistry` (`register` + `resolve_startup_modules` + `resolve_shutdown_modules`) and run `check_and_record_route` over module routes then app routes before printing `__RUME_ROUTES_JSON_START__` (matching `build_router` preflight semantics and failing closed on module cycles or duplicate routes).
4. **Lifecycle consolidation (`crates/app/src/app/lifecycle.rs` & `crates/testing/src/ext.rs`):**
   - Move the duplicated `ModuleRegistry` resolution, module `on_start` / rollback-on-failure, `on_ready` / rollback-on-failure, and reverse-order `on_shutdown` + module `on_shutdown` timeout runner out of `builder.rs` (`:1194-1396`) and `ext.rs` (`:45-169`) into `rume_app::app::lifecycle` (with `///` doc comments noting shared ownership by `RumeBuilder` and `rume-testing`'s `RumeTestingExt`).
   - Call those shared lifecycle helpers from both `RumeBuilder::run_with_bound_listener` (passing the shutdown `watch::Receiver<bool>` for `tokio::select!`) and `RumeTestingExt::build_test_app` (preserving the synchronous-drop `std::thread::spawn` + current-thread runtime teardown closure).

## Constraints honoured

- `rules/70-code-rationale.md`: `Why:` comment on `build_router_with_ctx` returning `(Router, AppContext)` (Axum `with_state(ctx)` snapshots `ctx` by value, including non-`Arc` `RouteRegistry`, so discarding the mutated `ctx` previously left `TestAppParts.ctx` and `on_ready`/`on_shutdown` stale), and on `run_inspect_mode` skipping `on_ready`/`on_shutdown` hooks while still enforcing module DAG and route-collision preflight.
- `crates/app/src/app/builder.rs:1484-1741` (`inspect_mode_*`, `preflight_failure_aborts_before_startup_hooks_run`, `fallible_startup_hook_failure_triggers_cleanup_hooks`) and `crates/testing/src/ext.rs:213-231` (`testing_ext_builds_test_app_and_runs_teardown_hooks`).
- `crates/core/tests/integration/test_server.rs` and `crates/core/tests/integration/architecture.rs`.

## Named tests (bodies omitted)

- `into_test_parts_and_on_ready_observe_module_routes_and_health_checks` (`crates/app/src/app/builder.rs` `#[cfg(test)]`) — registers a module adding route `/mod-items` and health check `"mod_db"`, runs `into_test_parts()` and `run_with_signal(...)` with an `on_ready` / `on_shutdown` spy, and asserts `ctx.route_registry` includes `/mod-items` and `ctx.health_checks` includes `"mod_db"`.
- `inspect_mode_rejects_module_cycle_and_duplicate_routes` (`crates/app/src/app/builder.rs` `#[cfg(test)]`) — runs `RumeBuilder::mode(ApplicationMode::Inspect)` with (a) cyclic module dependencies and (b) colliding app/module routes, asserting both return `InvalidInput` errors.
- `build_test_app_shares_rollback_on_on_ready_failure` (`crates/testing/src/ext.rs` `#[cfg(test)]`) — registers two modules (`alpha`, `beta` depending on `alpha`) and a failing `on_ready` hook, calls `build_test_app().await`, and asserts `beta` then `alpha` `on_shutdown` hooks ran in reverse topological order and lifecycle reached `Terminated`.

## Verification

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -j 2 -- -D warnings
cargo test -p rume-app --all-features -j 2
cargo test -p rume-testing --all-features -j 2
cargo test -p rume --test integration --all-features test_server -j 2
cargo test -p rume --test integration --all-features persistence -j 2
```

Expected: 0 warnings and all app/testing/integration tests pass.

## Commit

Only when the user asks:

```text
refactor(app): sync finalized AppContext, validate Inspect mode, and consolidate lifecycle runner
```

## Done-when

- [ ] `RumeBuilder` has no `state: S` field and returns finalized `AppContext` via `build_router_with_ctx` to `into_test_parts`, `run_with_signal`, and `run_with_listener_and_signal`.
- [ ] `run_inspect_mode` validates `ModuleRegistry` DAG and `check_and_record_route` before returning or printing routes.
- [ ] `crates/app/src/app/lifecycle.rs` is shared by both `builder.rs` and `crates/testing/src/ext.rs`.
- [ ] All verification commands exit 0 with 0 warnings.
