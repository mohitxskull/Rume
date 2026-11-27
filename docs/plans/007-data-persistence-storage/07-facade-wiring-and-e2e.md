# Task 07 — Facade Wiring & End-to-End Verification (`rume`)

## Goal

Wire all Milestone C primitives, `#[derive(FromRow)]`, and the `s3` feature into the `rume` facade (`crates/core`), add end-to-end integration tests exercising the full stack, and run the complete workspace verification suite.

## Files touched

- `crates/core/Cargo.toml`
- `crates/core/src/lib.rs`
- `crates/core/src/prelude.rs`
- `crates/core/tests/integration.rs`
- `crates/core/tests/integration/milestone_c_e2e.rs` (new)

## Deliverable

1. `crates/core/Cargo.toml`: forward `s3 = ["rume-data/s3"]` feature.
2. `crates/core/src/lib.rs` & `crates/core/src/prelude.rs`:
   - Re-export `rume_data::storage`, `rume_http::pagination`, `rume_http::upload`, and `rume_macros::FromRow`.
   - Add `FromRow`, `DynamicSelect`, `OffsetPage`, `CursorPage`, `ReadWritePool`, `SoftDeletable`, `OptimisticLock`, `ObjectStore`, `ValidatedMultipart` to `rume::prelude` (or `rume::persistence` / `rume::storage` / `rume::http` as appropriate, keeping `prelude` clean).
3. `crates/core/tests/integration/milestone_c_e2e.rs`: end-to-end integration suite exercising:
   - `#[derive(FromRow)]` + `DynamicInsert` / `DynamicSelect` + `OffsetPage` / HMAC `CursorPage` over real in-memory SQLite + Axum HTTP handler (`OffsetPageQuery`, `PaginatedResponse`).
   - `ReadWritePool` routing writes to primary, reads to replica, and honoring RYOW fallback window.
   - `SoftDeletable` + `OptimisticLock` returning `AppError::Conflict` (`409` RFC 9457 Problem Details) from an Axum handler on stale version.
   - `SeedRegistry` running dependent seeders idempotently on SQLite.
   - `ValidatedMultipart` Axum upload handler storing uploaded bytes into `LocalFsObjectStore` and reading them back.
   - `S3ObjectStore` `put` / `get` / `head` / `delete` / `presign_get` verified against `wiremock`.

## Constraints honoured

- `crates/core/src/lib.rs`: `#![forbid(unsafe_code)]`; preserve all existing re-exports and stability tier docs.
- `rules/40-verification.md` & `rules/80-quality-bar.md`: run the whole verification suite after the final edit with zero warnings and zero skipped/relaxed assertions.

## Named tests (bodies omitted)

- `crates/core/tests/integration/milestone_c_e2e.rs`:
  - `e2e_from_row_dynamic_query_and_pagination_over_http`
  - `e2e_read_write_pool_routing_and_ryow_fallback`
  - `e2e_soft_delete_and_optimistic_lock_returns_409_conflict`
  - `e2e_seed_registry_idempotent_with_dependencies`
  - `e2e_multipart_upload_into_local_fs_object_store`
  - `e2e_s3_object_store_with_wiremock_and_presigned_url`

## Verification

```bash
cargo fmt --all --check
cargo deny check licenses
cargo clippy -j 2 --workspace --all-targets --all-features -- -D warnings
cargo test -j 2 --workspace --lib --all-features
cargo test -j 2 -p rume-macros --all-features
cargo test -j 2 -p rume --test integration --all-features
cargo test -j 2 --workspace --doc --all-features
```

Expected: all workspace formatting, license, clippy, unit, macro, integration, and doc tests pass with zero warnings.

## Commit

Only when the user asks:

```text
feat(core): wire Milestone C persistence, pagination, storage, and FromRow into rume facade
```

## Done-when

- [ ] `rume` facade re-exports `FromRow`, `storage`, `pagination`, `upload`, and `s3` feature.
- [ ] `milestone_c_e2e.rs` integration tests pass.
- [ ] Full workspace verification suite passes cleanly with zero warnings.
