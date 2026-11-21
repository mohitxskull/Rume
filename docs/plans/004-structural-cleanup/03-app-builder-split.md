# Task 03 — Split `crates/app/src/app/builder.rs`

## Goal

Split `crates/app/src/app/builder.rs` (2,359 lines, "Rume application builder") into
`crates/app/src/app/builder/{mod,middleware,router,tests}.rs` — behavior-preserving;
`cargo build` + `cargo test -p rume-app` green.

## Files touched

- `crates/app/src/app/builder.rs` — deleted; replaced by:
- `crates/app/src/app/builder/mod.rs` — module docs (builder.rs:1-4); `Rume` + `impl Rume<(), (), ()>`
  (202-237) and `impl<E, C, S> Rume<E, C, S>` (224-237); `ReadyHook`/`ShutdownHook` type aliases
  (242-254); `TestAppParts` (260-274); `RumeBuilder` struct + `Clone`/`Debug` impls (278-426);
  `impl<E, C, S> RumeBuilder<E, C, S>` core methods (321-846): `new`, `record_route`,
  `module`/`register_module`, `get`/`post`/`put`/`patch`/`delete`/`any`/`resource`/`nest`/`merge`/
  `layer`/`fallback`/`fallback_service`/`route_service`/`route`/`route_spec` (the router-mounting
  methods live in `router.rs`), `on_ready`/`on_shutdown`, `lifecycle`, `availability`,
  `paths_ref`, `health_checks`, `shutdown_timeout`, `run`, `run_with_signal`,
  `run_with_listener_and_signal`, `run_with_bound_listener`, `prepare_serve`, `effective_mode`,
  `run_inspect_mode`; `PreparedServe` (1208-1221); `#[cfg(test)] mod tests;` → `builder/tests.rs`.
  `BuildError` is defined here (used by router/lifecycle); `IntoBindAddr`/`IntoHookResult` defined here.
- `crates/app/src/app/builder/middleware.rs` — `MiddlewareConfigError` + `Display`/`Error`/`From`
  (builder.rs:28-72); `use_middleware` (805-808), `use_group` (833-836), `middleware` (788-795),
  `group` (819-826); `validate_middleware_config` (1255-1308).
- `crates/app/src/app/builder/router.rs` — `get` (377-392), `post` (395-410), `put` (413-428),
  `patch` (431-446), `delete` (449-464), `any` (467-482), `resource` (485-498), `nest` (501-526),
  `merge` (529-540), `layer` (543-555), `fallback` (558-565), `fallback_service` (568-580),
  `route_service` (583-603), `route` (639-650), `route_spec` (653-666), `openapi` (672-685);
  `build_router` (854-859), `build_router_with_ctx` (863-869), `build_router_with_resolved`
  (873-987); free helpers `collect_and_validate_routes` (1223-1253), `merge_module_router`
  (1312-1332), `parse_axum_merge_conflict` (1334-1348), `normalize_route_template` (1353-1368,
  `pub(crate)` — called by `builder/tests.rs` test `normalize_route_template_normalizes_captures_and_wildcards`),
  `methods_conflict` (1370-1380), `check_and_record_route` (1382-1400); `BuiltRouterWithCtx`
  (1206-1207). `run_inspect_mode` is `pub(crate)` — called by `builder/tests.rs` inspect tests.
- `crates/app/src/app/builder/tests.rs` — the `#[cfg(test)] mod tests` moved verbatim (builder.rs:1401-2218).

## Deliverable

`app::builder` still re-exports `BuildError`, `IntoBindAddr`, `MiddlewareConfigError`, `Rume`,
`RumeBuilder` (`app/mod.rs:13`); `crate::app::builder::{BuildError, ReadyHook, ShutdownHook,
RumeBuilder}` resolve for `crates/testing/src/ext.rs:6`, `crates/testing/src/server.rs:14`,
`crates/testing/src/app.rs:41-42`, `crates/app/src/app/lifecycle.rs:15`,
`crates/core/tests/integration/architecture.rs:272,373` (`rume::app::builder::BuildError::DuplicateRoute`).
No behavior changes; all method bodies move as-is.

## Constraints honoured

- `crates/app/src/app/mod.rs:13` `pub use builder::{BuildError, IntoBindAddr, MiddlewareConfigError, Rume, RumeBuilder};`
  unchanged — resolves via `builder/mod.rs` re-exports (`MiddlewareConfigError` from `middleware.rs`).
- `crates/testing/src/ext.rs:6`, `server.rs:14`, `app.rs:41-42`, `crates/app/src/app/lifecycle.rs:15`
  (`use crate::app::builder::{BuildError, ReadyHook, ShutdownHook};` — mod.rs defines ReadyHook/ShutdownHook).
- `crates/core/tests/integration/architecture.rs:272,373` unchanged.
- No behavior change; `cargo build` + `cargo test -p rume-app` green.

## Named tests (bodies omitted — moved from `crates/app/src/app/builder.rs:1401-2218`)

From `builder/tests.rs`:
- `test_ctx`, `mw_add_route` (helpers used by the below tests).
- `known_middleware_applied_successfully` — asserts a registered+used middleware applies to routes.
- `unknown_middleware_via_use_middleware_is_rejected`, `unknown_group_via_use_group_is_rejected` —
  `MiddlewareConfigError::UnknownMiddleware/UnknownGroup` surfaces.
- `duplicate_middleware_registration_is_rejected`, `duplicate_group_definition_is_rejected` —
  `DuplicateMiddleware/DuplicateGroup`.
- `group_with_unknown_member_is_rejected` — `UnknownGroupMember`.
- `middleware_applied_in_registration_order`, `group_members_applied_in_declaration_order`,
  `middleware_individual_execution_order_during_request`,
  `middleware_group_execution_order_during_request`,
  `middleware_combined_individual_and_groups_execution_order` — asserts middleware/group execution order
  matches the documented onion model (individual outermost → group → handler).
- `run_returns_error_on_misconfigured_middleware` — `run_with_signal` fails on unknown middleware.
- `error_display_messages_are_descriptive` — asserts Display strings of `MiddlewareConfigError` variants.
- `inspect_mode_does_not_run_on_ready_hooks` / `inspect_mode_does_not_run_on_shutdown_hooks` /
  `inspect_mode_sets_booted_live_then_returns` — asserts `run_inspect_mode` validates config but skips hooks.
- `preflight_failure_aborts_before_startup_hooks_run` — module preflight failure aborts before `on_ready`.
- `fallible_startup_hook_failure_triggers_cleanup_hooks` — `on_ready` failure runs registered shutdown hooks.
- `app_builder_topological_startup_and_reverse_shutdown_on_failure` — module startup/shutdown order.
- `module_route_conflicting_with_app_route_is_rejected` — `BuildError::DuplicateRoute`.
- `conflicting_capture_parameter_names_are_rejected_as_duplicate_route` — normalized-template conflict.
- `normalize_route_template_normalizes_captures_and_wildcards` — asserts `{id}`/`:id` → `{}`, `{*rest}` → `{*}`.
- `disjoint_methods_on_identical_template_are_allowed` / `any_route_still_conflicts_with_specific_method_on_same_path` —
  route-conflict semantics.
- `opaque_method_router_with_disjoint_method_is_allowed` — `RouteInfo::unknown_method()` handling.
- `into_test_parts_and_on_ready_observe_module_routes_and_health_checks` — `TestAppParts` contents.

## Verification

```
cargo build
cargo test -p rume-app
```

## Commit

Only when the user asks:

```
refactor(app): split app/builder.rs into app/builder/ middleware and router modules
```

## Done-when

- [ ] `crates/app/src/app/builder/` contains `mod.rs`, `middleware.rs`, `router.rs`, `tests.rs`;
  `crates/app/src/app/builder.rs` deleted.
- [ ] `rume::app::builder::{BuildError, Rume, RumeBuilder}` and `rume_app::app::builder::{BuildError,
  RumeBuilder}` (testing) still resolve; `rume::app::builder::BuildError::DuplicateRoute` resolves.
- [ ] `cargo build` + `cargo test -p rume-app` green.
