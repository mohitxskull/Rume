# Task 01 — Dependencies, Error Variants, and Compiling Skeleton

## Goal

Pin new workspace dependencies, wire the `rume-http` -> `rume-data` DAG edge and `axum/multipart` feature, add `AppError::Conflict` (`409`) and `PersistenceError::VersionConflict`, and scaffold compiling `todo!()` skeletons for all Milestone C modules so subsequent tasks read concrete types from the compiler.

## Files touched

- `Cargo.toml`
- `crates/foundation/src/error.rs`
- `crates/data/Cargo.toml`
- `crates/macro/Cargo.toml`
- `crates/data/src/lib.rs`
- `crates/data/src/persistence/mod.rs`
- `crates/data/src/persistence/error.rs`
- `crates/data/src/persistence/builder.rs` (new skeleton)
- `crates/data/src/persistence/pagination.rs` (new skeleton)
- `crates/data/src/persistence/read_write.rs` (new skeleton)
- `crates/data/src/persistence/conventions.rs` (new skeleton)
- `crates/data/src/persistence/seed.rs` (new skeleton)
- `crates/data/src/storage/mod.rs` (new skeleton)
- `crates/data/src/storage/local.rs` (new skeleton)
- `crates/data/src/storage/s3.rs` (new skeleton, `#[cfg(feature = "s3")]`)
- `crates/macro/src/lib.rs`
- `crates/macro/src/from_row.rs` (new skeleton)
- `crates/http/Cargo.toml`
- `crates/http/src/lib.rs`
- `crates/http/src/pagination.rs` (new skeleton)
- `crates/http/src/upload.rs` (new skeleton)
- `crates/cli/src/cli.rs`
- `crates/cli/src/commands/db/mod.rs`

## Deliverable

1. Root `Cargo.toml`: pin `sea-query = { version = "=1.0.2", default-features = false, features = ["backend-sqlite", "backend-postgres", "backend-mysql", "thread-safe"] }`.
2. `crates/foundation/src/error.rs`: add `AppError::Conflict { code: &'static str, message: String }`, `AppError::conflict(msg: impl Into<String>)`, and RFC 9457 `409 Conflict` response mapping in `status_code()` and `into_response_in_env()` (rendering the developer-intentional `message` verbatim, matching `AppError::Public`).
3. `crates/data/src/persistence/error.rs`: add `PersistenceError::VersionConflict { entity: String, expected: i64, actual: Option<i64> }` and `impl From<PersistenceError> for rume_foundation::error::AppError` (mapping `VersionConflict` to `AppError::Conflict` with a sanitized message that does not expose internal SQL or schema details, and other variants to `AppError::internal`).
4. `crates/data/Cargo.toml`: add `sea-query.workspace = true`, `hmac.workspace = true`, `base64.workspace = true`, and `subtle.workspace = true` as unconditional `[dependencies]` (needed by `CursorCodec`), plus optional `reqwest.workspace = true`, `hex.workspace = true`, `url.workspace = true`, `chrono.workspace = true`, and feature `s3 = ["dep:reqwest", "dep:hex", "dep:url", "dep:chrono"]`.
5. `crates/macro/Cargo.toml`: add `rume-data.workspace = true` under `[dev-dependencies]` for `#[derive(FromRow)]` runtime and `trybuild` tests.
6. `crates/http/Cargo.toml`: add `rume-data.workspace = true` and `axum = { workspace = true, features = ["multipart"] }`.
7. Compiling `todo!()` skeletons for `rume_macros::FromRow`, `rume_data::persistence::{builder, pagination, read_write, conventions, seed}`, `rume_data::storage`, `rume_http::{pagination, upload}`, and `DbCommands::Seed` in `crates/cli`.

## Constraints honoured

- `rules/10-tech-stack.md` & `deny.toml`: concrete `=x.y.z` pin for `sea-query = "=1.0.2"`; verify `cargo deny check licenses` passes cleanly.
- `README.md` §Global constraints: `#![forbid(unsafe_code)]`, strict 7-layer DAG (`rume-http` L4 -> `rume-data` L2 is downward; `rume-data` L2 -> `rume-foundation` L0 is downward).
- `crates/foundation/src/error.rs`: preserve existing `ProblemDetails` JSON shape (`type`, `title`, `status`, `detail`, `instance`, `code`, `request_id`).
- `crates/cli/src/cli.rs`: keep existing `DbCommands::{Status, Migrate, Rollback}` and hidden `db:*` aliases intact.

## Named tests (bodies omitted)

- `crates/foundation/src/error.rs`: `conflict_error_maps_to_409_problem_details` — asserts `AppError::conflict("version mismatch")` produces HTTP `409` with `application/problem+json` and `code: "CONFLICT"`.
- `crates/data/src/persistence/error.rs`: `version_conflict_converts_to_app_error_conflict` — asserts `PersistenceError::VersionConflict` converts to `AppError::Conflict` (`409`).

## Verification

```bash
cargo deny check licenses
cargo check -j 2 --workspace --all-targets --all-features
cargo test -j 2 -p rume-foundation --lib
cargo test -j 2 -p rume-data --lib --all-features
```

Expected: zero license errors, entire workspace compiles with `todo!()` skeletons, and foundation/data error tests pass.

## Commit

Only when the user asks:

```text
chore(data,http,macro): scaffold Milestone C dependencies, error variants, and module skeletons
```

## Done-when

- [ ] `sea-query = "=1.0.2"` is pinned in `Cargo.toml` and `cargo deny check licenses` passes.
- [ ] `AppError::Conflict` and `PersistenceError::VersionConflict` compile and pass unit tests.
- [ ] `cargo check -j 2 --workspace --all-targets --all-features` succeeds with zero errors.
