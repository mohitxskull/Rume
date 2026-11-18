# 12-rc05-builder-and-error.md

## Goal

Fix RC-05 (builder.rs + error.rs), three defects:

1. `builder.rs` Inspect mode: `build_router` reads `route_registry` into `seen_routes` (~line 997) **before** module `register_routes` runs (~line 1002), so module routes are invisible to duplicate-route detection and the registry is stale when routes are registered. Fix: register module routes before reading `route_registry` for `seen_routes`/duplicate detection.
2. `builder.rs` dead `environment` field (line 267): populated in `new()` via `Environment::from_env()` but never read in `build_router`/`run`; `environment()` setter and `environment_ref()` getter are unused anywhere in src. Fix: remove the dead field and its accessors (confirmed no callers via grep).
3. `error.rs` `is_running_in_test` branch (lines 353-360): `IntoResponse` forces the test environment in test builds, hiding development/production rendering behaviour. Fix: remove the branch so `IntoResponse` uses `Environment::from_env()` consistently. (Also error.rs:88 `span_id` Debug format — `format!("{:?}", span.id())` — use a stable span identifier instead of `Id`'s Debug.)

## Files touched

- `crates/core/src/app/builder.rs` — `build_router` module-route ordering (~lines 997-1026); remove `environment` field + `environment()`/`environment_ref()` accessors.
- `crates/core/src/error.rs` — `IntoResponse` impl (remove `is_running_in_test` branch, lines 353-360); `InternalSource::new` span_context (line 85-89).

## Constraints honoured

- Root cause, not symptom: register before reading; delete dead code rather than leaving it; stop special-casing test env.
- No new dependencies, no `unsafe`.
- Confirm no callers of `environment_ref()`/`environment()` before removal (grep showed none in src).
- Record `Why:` comments: inspect ordering, dead-field removal, env branch removal.

## Done when (checkable by a command)

- `cargo test -p rume-core` passes (builder tests + error tests).
- `cargo clippy -p rume-core --all-targets` and `cargo fmt --all --check` clean.
- Grep confirms no remaining references to `environment` field / `environment_ref()` / `environment()`.
- `build_router` registers module routes before computing `seen_routes`.
- `IntoResponse` no longer branches on `is_running_in_test()`.

## Verification (fails-before test idea)

Tests:
1. **Inspect ordering:** register a module route at `/m/x` and an app route at `/m/x` (same method+path) → `build_router` must return `DuplicateRoute`. Before the fix the module route isn't in `seen_routes` yet, so no duplicate error → test fails.
2. **Dead field removed:** `RumeBuilder` no longer has an `environment` field / `environment_ref` / `environment` setter (compile check).
3. **error.rs:** `IntoResponse` in a non-development env returns the generic "An unexpected error occurred." detail even when compiled in test cfg (no special-casing). Before the fix test-env forced development-like behavior → test fails.
