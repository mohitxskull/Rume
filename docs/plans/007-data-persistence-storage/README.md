# Plan 007 — Milestone C: Data, Persistence & Storage

## Goal

Close all six Milestone C data, persistence, and storage gaps cleanly and idiomatically, executed **strictly sequentially on `main`** (one task at a time):

1. **`#[derive(FromRow)]` Proc-Macro (`rume-macros`)** — Compile-time row-mapping derive supporting named structs, `#[from_row(rename = "...")]`, `#[from_row(default)]`, `#[from_row(skip)]`, `#[from_row(flatten)]`, `#[from_row(json)]`, `#[from_row(crate = "...")]`, generic structs, and `trybuild` compile-fail diagnostics.
2. **Dynamic Query Builder & Pagination (`rume-data` + `rume-http`)** — Composable typed dynamic query builder wrapping `sea-query = "=1.0.2"` across `Sqlite`/`Postgres`/`MySql` dialects (parameterized `DbValue` bindings, zero raw SQL string concatenation) plus `OffsetPage<T>` / `OffsetPageRequest` and `CursorPage<T>` / `CursorPageRequest` (opaque base64url and HMAC-SHA256 signed cursors) with `HasOpenApiSchema` impls and `rume-http` query extractors.
3. **Read/Write Replica Pool Routing (`rume-data`)** — `ReadWritePool<B>` wrapping a primary `DatabasePool<B>` and 0..N replica `DatabasePool<B>`s with round-robin health-aware read routing, explicit `.primary()` / `.replica()` / `.begin_tx()` routing, and a configurable read-your-own-writes (RYOW) fallback window.
4. **Soft Deletes & Optimistic Locking Conventions (`rume-data` + `rume-foundation`)** — `SoftDeletable` and `OptimisticLock` helpers (`where_not_deleted`, `soft_delete_update`, CAS `WHERE version = ?` returning `PersistenceError::VersionConflict` -> `AppError::Conflict` / HTTP `409 Conflict`) integrated with the dynamic query builder.
5. **Database Seeding (`rume-data` + `rume-cli`)** — `Seeder` trait, `SeedRegistry` (dependency-ordered topological execution, cycle detection, idempotency, per-seeder transaction wrapping), and `SeedReport` in `rume-data`, wired to `rume db seed` in `crates/cli`.
6. **Multipart File Uploads & Object Storage (`rume-data` + `rume-http`)** — Unified `ObjectStore` trait (`put`, `get`, `head`, `delete`, `list`, `presign_get`, `presign_put`) with path-traversal-safe `LocalFsObjectStore` and pure-Rust SigV4 `S3ObjectStore` (behind `s3` feature), plus `rume-http::upload` streaming multipart extractor with size/MIME/extension validation and filename sanitization.

## Current state (read before executing)

Facts verified against the tree at plan time:

- **Workspace pins (`Cargo.toml` / `deny.toml`)**:
  - Edition `2024`, MSRV `1.96`, `#![forbid(unsafe_code)]` on all library crates, `deny.toml` license allow-list (`Apache-2.0`, `MIT`, `BSD-2-Clause`, `BSD-3-Clause`, `ISC`, etc.), `multiple-versions = "warn"`.
  - Already pinned in `Cargo.lock`: `axum = "=0.8.9"`, `reqwest = "=0.13.5"`, `hmac = "=0.13.0"`, `sha2 = "=0.11.2"`, `hex = "=0.4.3"`, `base64 = "=0.23.1"`, `subtle = "=2.6.1"`, `chrono = "=0.4.45"`, `url = "=2.5.9"`, `serde_json = "=1.0.154"`, `syn = "=3.0.10"`, `proc-macro2 = "=1.0.107"`, `quote = "=1.0.47"`, `trybuild = "=1.0.125"`.
  - New crate to pin: `sea-query = "=1.0.2"` (`MIT OR Apache-2.0`, MSRV `1.88.0`, features `["backend-sqlite", "backend-postgres", "backend-mysql"]`).
- **Persistence (`crates/data/src/persistence/`)**:
  - `row.rs` defines `FromDbValue`, `DbRow`, and `FromRow` (`fn from_row(row: &DbRow) -> Result<Self, PersistenceError>`).
  - `pool.rs` defines `DatabasePool<B>` with `fetch_all_as`, `fetch_optional_as`, `fetch_one_as`, `execute_params`, `transaction`, and `DatabaseHealthCheck`.
  - `backend/mod.rs` defines `DatabaseBackend`, `DatabaseTransaction`, `DbValue`, and `validate_sql_identifier`.
  - `error.rs` defines `PersistenceError` (currently no `VersionConflict` variant).
- **Errors (`crates/foundation/src/error.rs`)**:
  - `AppError` currently has `NotFound`, `BadRequest`, `Unauthorized`, `Forbidden`, `Validation`, `Internal`, `Public` (no `Conflict` variant yet).
- **Proc-macros (`crates/macro/src/`)**:
  - `lib.rs` exports `sql_assets!`, `embed_migrations!`, and `#[derive(HasOpenApiSchema)]`.
  - `openapi_schema.rs` implements `parse_openapi_crate_path` and `trybuild` UI tests live in `crates/macro/tests/compile_fail.rs` + `crates/macro/tests/ui/`.
- **HTTP (`crates/http/src/`)**:
  - `Cargo.toml` depends on `rume-foundation`, `rume-observability`, `rume-security`, `axum` (without `"multipart"` feature yet), and does not yet depend on `rume-data`.
  - `validation.rs` defines `ValidatedJson`, `ValidatedQuery`, `ValidatedPath`, `ValidatedForm` returning `AppError`.
- **CLI (`crates/cli/src/commands/db/mod.rs`, `crates/cli/src/cli.rs`)**:
  - `DbCommands` has `Status`, `Migrate`, `Rollback` (plus hidden `db:status`, `db:migrate`, `db:rollback` shortcuts).
- **Facade (`crates/core/src/lib.rs`, `crates/core/src/prelude.rs`)**:
  - Re-exports `persistence`, `HasOpenApiSchema`, `embed_migrations`, `sql_assets`, etc.

## Design Rulings

Autonomous engineering rulings for Milestone C (no open questions):

1. **`sea-query = "=1.0.2"` for dynamic SQL generation:** Pin `sea-query = { version = "=1.0.2", default-features = false, features = ["backend-sqlite", "backend-postgres", "backend-mysql", "thread-safe"] }` in root `Cargo.toml` and `crates/data/Cargo.toml`. Convert `sea_query::Value` to/from `DbValue` inside `rume-data::persistence::builder` and execute generated `(sql, Vec<DbValue>)` through `DatabasePool` / `DatabaseTransaction` (never raw string concatenation).
2. **Pure-Rust SigV4 `S3ObjectStore` instead of `object_store` crate:** Implement `S3ObjectStore` in `rume-data::storage::s3` (gated behind `s3` feature, forwarded by `rume`) using workspace-pinned `reqwest = "=0.13.5"`, `hmac = "=0.13.0"`, `sha2 = "=0.11.2"`, `hex = "=0.4.3"`, `chrono = "=0.4.45"`, and `url = "=2.5.9"`. This avoids pulling `object_store`'s `aws-lc-rs`/`ring` C dependencies and duplicate `reqwest` versions (`deny.toml` `multiple-versions = "warn"`), keeps `#![forbid(unsafe_code)]`, and supports presigned GET/PUT URLs natively.
3. **`rume-http` -> `rume-data` dependency for pagination & OpenAPI:** Add `rume-data.workspace = true` to `crates/http/Cargo.toml` (L4 -> L2, strictly downward in the 7-layer DAG). Pagination core types (`OffsetPageRequest`, `OffsetPage<T>`, `CursorPageRequest`, `CursorPage<T>`, `CursorCodec`) live in `rume-data::persistence::pagination` so data-layer queries return pages directly, while `rume-http::pagination` adds `HasOpenApiSchema` impls (or `#[cfg]`-free impls in `rume-http` via wrapper/trait or `rume-http` newtypes) and Axum `FromRequestParts` query extractors.
   - *Orphan-rule ruling:* Because `OffsetPage<T>` and `CursorPage<T>` are defined in `rume-data` and `HasOpenApiSchema` is defined in `rume-http`, `rume-http` cannot implement `HasOpenApiSchema` directly on `rume_data::persistence::OffsetPage<T>` without violating Rust's orphan rule. Therefore, define `HasOpenApiSchema`-implementing response/query wrappers or `PageSchemaExt` in `rume-http::pagination` (`OffsetPageQuery`, `CursorPageQuery`, `PaginatedResponse<T>` / `CursorPaginatedResponse<T>` with `From<OffsetPage<T>>` / `From<CursorPage<T>>`) so OpenAPI schema generation works seamlessly without moving `HasOpenApiSchema` down to `rume-foundation`.
4. **`#[derive(FromRow)]` default crate path:** Default `#[from_row(crate = "...")]` to `::rume::persistence` (matching `rume` facade users), and support `#[from_row(crate = "rume_data::persistence")]` (or any custom path) for internal workspace crates and `trybuild` tests.
5. **`axum` `"multipart"` feature for uploads:** Enable `features = ["multipart"]` on `axum` in `crates/http/Cargo.toml` (`multer`, MIT-licensed, zero `unsafe`). Expose `MultipartUploadConfig`, `UploadedFile`, and `ValidatedMultipart` in `rume-http::upload`.
6. **Optimistic locking & soft-delete error mapping:** Add `AppError::Conflict { code: &'static str, message: String }` (`409 Conflict`) to `rume-foundation::error` and `PersistenceError::VersionConflict { entity: String, expected: i64, actual: Option<i64> }` + `impl From<PersistenceError> for AppError` in `rume-data::persistence::error`.
7. **Synchronous/async `rume db seed` CLI ergonomics:** Keep `Seeder` trait and `SeedRegistry` in `rume-data::persistence::seed` so applications register their own `Seeder` implementations programmatically, and have `rume db seed` execute SQL seed files from `seeds/` (ordered lexicographically, transaction-wrapped, idempotent via `_rume_seeds` ledger table) using the same `DatabaseConfig` discovery as `rume db migrate`.

## Global constraints

- **Workspace invariants**: `#![forbid(unsafe_code)]` in all library crates; Rust Edition 2024; MSRV `1.96`; concrete version pins (`=x.y.z`) in root `Cargo.toml`; all dependencies must pass `cargo deny check licenses` against `deny.toml`.
- **Strict 7-layer DAG**: L0 `rume-foundation` -> L1 `rume-observability` -> L2 `rume-data` -> L3 `rume-messaging`/`rume-security`/`rume-enterprise` -> L4 `rume-http` -> L5 `rume-app` -> L6 `rume` (`crates/core`) + `rume-macros`.
- **Build concurrency**: Every `cargo` command MUST pass `-j 2`.
- **Zero-warning bar** (`rules/80-quality-bar.md`): `cargo fmt --all --check` and `cargo clippy -j 2 --workspace --all-targets --all-features -- -D warnings` must pass with zero warnings.
- **Execution mode (strictly sequential on `main`)**: Execute tasks `01` through `07` strictly in order on `main`, one implementer subagent at a time (`rules/30-git.md`, `rules/50-agent-conduct.md`).
- **TDD & fails-before** (`rules/50-agent-conduct.md`, `rules/80-quality-bar.md`): Task `01` scaffolds the compiling `todo!()` skeleton; tasks `02`–`06` write tests first against the skeleton (confirming fails-before), then implement minimal clean code; task `07` wires the facade and runs full end-to-end verification.
- **Commit discipline** (`rules/30-git.md`): Conventional Commits; no commits unless explicitly asked or announced up front by the controller.

## Constraints honoured

- **Row decoding semantics**: `crates/data/src/persistence/row.rs` (`DbRow::get`, `DbRow::get_opt`, `FromDbValue` error taxonomy preserving `ColumnNotFound` vs `DecodeError`).
- **SQL injection & identifier safety**: `crates/data/src/persistence/backend/mod.rs` (`validate_sql_identifier`), `crates/data/src/persistence/pool.rs` (parameterized `DbValue` execution).
- **Existing `AppError` RFC 9457 contract**: `crates/foundation/src/error.rs` (`ProblemDetails`, `status_code()`, `into_response_in_env`).
- **Existing CLI `rume db` UX**: `crates/cli/src/commands/db/mod.rs` (`discover_local_migrations`, `DatabaseConfig::from_env` / config loading, `TableBuilder` output).
- **Existing proc-macro patterns**: `crates/macro/src/openapi_schema.rs` (`crate` attribute override) and `crates/macro/tests/compile_fail.rs` (`trybuild`).
- **Rules**: `rules/10-tech-stack.md`, `rules/40-verification.md`, `rules/50-agent-conduct.md`, `rules/70-code-rationale.md`, `rules/80-quality-bar.md`, `rules/85-minimalism.md`, `rules/testing.md`.

## Task index

- `01-deps-and-skeleton.md` — Pin `sea-query = "=1.0.2"`, enable `axum/multipart` and `rume-http` -> `rume-data` DAG edge, add `AppError::Conflict` + `PersistenceError::VersionConflict`, and scaffold compiling skeletons across `rume-macros`, `rume-data`, `rume-http`, and `rume-cli`.
- `02-derive-from-row-macro.md` — Implement `#[derive(FromRow)]` in `rume-macros` with `rename`, `default`, `skip`, `flatten`, `json`, `crate`, generic support, and `trybuild` compile-fail tests.
- `03-query-builder-and-pagination.md` — Implement `sea-query`-backed dynamic query builder and `OffsetPage` / `CursorPage` pagination in `rume-data`, plus OpenAPI-aware query extractors and response wrappers in `rume-http`.
- `04-read-write-pool-and-conventions.md` — Implement `ReadWritePool<B>` (primary/replica routing + RYOW fallback window) and `SoftDeletable` / `OptimisticLock` helpers in `rume-data`.
- `05-database-seeding-and-cli.md` — Implement `Seeder`, `SeedRegistry`, `SeedReport` in `rume-data` and `rume db seed` in `rume-cli`.
- `06-object-storage-and-multipart.md` — Implement `ObjectStore`, `LocalFsObjectStore`, and pure-Rust SigV4 `S3ObjectStore` in `rume-data::storage`, plus `ValidatedMultipart` upload extractor in `rume-http::upload`.
- `07-facade-wiring-and-e2e.md` — Wire all new APIs and features into `rume` (`crates/core`), add end-to-end integration tests, and run full workspace verification.

## Done-when checklist

- [ ] `sea-query = "=1.0.2"` is pinned in root `Cargo.toml` and `cargo deny check licenses` passes.
- [ ] `#[derive(FromRow)]` supports named structs, `rename`, `default`, `skip`, `flatten`, `json`, `crate`, generics, and passes `trybuild` UI tests.
- [ ] `rume-data::persistence::builder` compiles dynamic `SELECT`/`INSERT`/`UPDATE`/`DELETE` queries via `sea-query` across SQLite/Postgres/MySQL dialects and executes them via `DatabasePool`/`DatabaseTransaction`.
- [ ] `rume-data::persistence::pagination` and `rume-http::pagination` provide `OffsetPage<T>` and HMAC-signed/opaque `CursorPage<T>` with `HasOpenApiSchema` and Axum extractors.
- [ ] `rume-data::persistence::read_write::ReadWritePool<B>` routes reads across healthy replicas, writes to primary, and honors the RYOW fallback window.
- [ ] `rume-data::persistence::conventions` provides `SoftDeletable` and `OptimisticLock` CAS helpers returning `PersistenceError::VersionConflict` -> `AppError::Conflict` (`409`).
- [ ] `rume-data::persistence::seed` and `rume db seed` execute idempotent, transaction-wrapped, dependency-ordered seeds.
- [ ] `rume-data::storage` provides `ObjectStore`, `LocalFsObjectStore` (path-traversal safe), and pure-Rust SigV4 `S3ObjectStore` (under `s3` feature), and `rume-http::upload` provides `ValidatedMultipart` with size/MIME/extension/filename guards.
- [ ] All verification commands below pass cleanly with zero warnings.

## Verification commands

```bash
cargo fmt --all --check
cargo deny check licenses
cargo clippy -j 2 --workspace --all-targets --all-features -- -D warnings
cargo test -j 2 --workspace --lib --all-features
cargo test -j 2 -p rume-macros --all-features
cargo test -j 2 -p rume --test integration --all-features
cargo test -j 2 --workspace --doc --all-features
```

## Flagged gaps

- **Live S3 / Postgres / MySQL container tests**: `S3ObjectStore` SigV4 signing, presigned URLs, and HTTP operations are verified hermetically against AWS test vectors and `wiremock` in unit/integration tests; live S3-compatible server tests (e.g. MinIO) are optional/env-gated if Docker is unavailable per `rules/testing.md`.
