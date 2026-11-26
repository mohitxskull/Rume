# Plan 007 Review — Milestone C: Data, Persistence & Storage

**Verdict:** `Changes Requested`
**Findings:** 0 Critical, 2 Major, 4 Minor

## Summary

Plan 007 (`docs/plans/007-data-persistence-storage/`) is well-structured, complies with `plan-authoring/SKILL.md` (all required sections in `README.md` and tasks `01`–`07`, no boilerplate/type-signature drift, concrete `Named tests` and `Verification` commands), and its core architectural rulings are sound:

- **`sea-query = "=1.0.2"` verified:** Exists on crates.io (`yanked: false`), licensed `MIT OR Apache-2.0` (compatible with `deny.toml`), MSRV `1.88.0` (compatible with workspace `1.96`), and with `default-features = false, features = ["backend-sqlite", "backend-postgres", "backend-mysql", "thread-safe"]` introduces **zero** transitive dependencies.
- **`rume-http` -> `rume-data` DAG legality:** `rume-http` (L4) -> `rume-data` (L2) is strictly downward in the 7-layer DAG; `rume-data` depends only on `rume-foundation` (L0) and `rume-observability` (L1), so no cycle is introduced. The orphan-rule ruling (`OffsetPageQuery`, `CursorPageQuery`, `PaginatedResponse<T>`, `CursorPaginatedResponse<T>` in `rume-http::pagination`) cleanly preserves `HasOpenApiSchema` without moving OpenAPI types down to `rume-foundation`.
- **Pure-Rust SigV4 `S3ObjectStore`:** Avoids `object_store`'s `aws-lc-rs`/`ring` C dependencies and duplicate `reqwest` versions (`deny.toml` `multiple-versions = "warn"`), while preserving `#![forbid(unsafe_code)]`.

Two Major dependency-wiring gaps in Tasks 01–03 would block compilation if executed verbatim and should be fixed before scaffolding.

---

## Findings

### Major

1. **M1 — `crates/data/Cargo.toml` missing/mis-scoped dependencies for `CursorCodec` (Tasks 01 & 03)**
   - **Where:** `01-deps-and-skeleton.md` Deliverable #4; `03-query-builder-and-pagination.md` Deliverable #2.
   - **Issue:** Task 01 adds `hmac.workspace = true` as **optional** behind `s3 = ["dep:reqwest", "dep:hmac", ...]` and omits `base64.workspace = true` and `subtle.workspace = true` from `crates/data/Cargo.toml`. However, Task 03 requires `CursorCodec` in `rume-data::persistence::pagination` (unconditional, not gated by `s3`) to use `hmac = "=0.13.0"`, `sha2 = "=0.11.2"`, `base64`, and `subtle::ConstantTimeEq`. Building `rume-data` without `--all-features` will fail.
   - **Fix:** In Task 01 (`crates/data/Cargo.toml`), make `hmac.workspace = true`, `base64.workspace = true`, and `subtle.workspace = true` non-optional `[dependencies]`, and remove `"dep:hmac"` from the `s3` feature list.

2. **M2 — `crates/macro/Cargo.toml` lacks `rume-data` dev-dependency for `#[derive(FromRow)]` tests (Tasks 01 & 02)**
   - **Where:** `02-derive-from-row-macro.md` `Files touched` & `Named tests` (`crates/macro/tests/from_row.rs`).
   - **Issue:** `crates/macro/Cargo.toml` currently only lists `serde_json` and `trybuild` in `[dev-dependencies]`. Runtime and `trybuild` tests for `#[derive(FromRow)]` using `#[from_row(crate = "rume_data::persistence")]` require `rume-data.workspace = true` in `crates/macro/Cargo.toml` `[dev-dependencies]` (note: `rume-data` does not depend on `rume-macros`, so a dev-dependency introduces no cycle), yet `crates/macro/Cargo.toml` is not listed in `Files touched`.
   - **Fix:** Add `crates/macro/Cargo.toml` to Task 01 (or Task 02) `Files touched` with `rume-data.workspace = true` under `[dev-dependencies]`.

---

### Minor

1. **m1 — `chrono` `clock` feature not enabled in root `Cargo.toml` (Tasks 01 & 06)**
   - Root `Cargo.toml` pins `chrono = { version = "=0.4.45", default-features = false, features = ["std"] }`. `S3ObjectStore` SigV4 timestamp generation should use `chrono::DateTime::<chrono::Utc>::from(std::time::SystemTime::now())` (works with `std` feature only) rather than `chrono::Utc::now()`, or explicitly enable `chrono/clock` under `s3`.

2. **m2 — `LocalFsObjectStore` symlink containment on `put` (Task 06)**
   - Canonicalizing an existing file catches symlinks on `get`/`head`/`delete`, but on `put` (when the target file does not yet exist) `LocalFsObjectStore` must canonicalize the nearest existing ancestor directory and/or reject any symlink component along the path inside `base_dir` before creating directories or renaming the temp file.

3. **m3 — `ValidatedMultipart` interaction with Axum `DefaultBodyLimit` (Task 06)**
   - `MultipartUploadConfig` defaults to `10 MiB`, while Axum's `DefaultBodyLimit` defaults to `2 MiB` and rejects larger bodies before `FromRequest` runs. Document and test `DefaultBodyLimit::disable()` (or `RequestBodyLimitLayer`) on upload routes exceeding `2 MiB`.

4. **m4 — `AppError::Conflict` redaction policy & `integration.rs` path (Tasks 01 & 07)**
   - Specify in Task 01 whether `AppError::Conflict { code, message }` renders `message` verbatim (like `AppError::Public`) or redacts in non-development environments (like `AppError::BadRequest`), ensuring `From<PersistenceError>` does not leak internal schema/table names if unredacted.
   - In Task 07 `Files touched`, the existing integration test root is `crates/core/tests/integration.rs` (not `main.rs`).
