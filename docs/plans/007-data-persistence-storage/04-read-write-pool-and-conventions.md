# Task 04 — Read/Write Replica Pool Routing, Soft Deletes & Optimistic Locking (`rume-data`)

## Goal

Implement `ReadWritePool<B>` (primary + 0..N replicas with health-aware round-robin read routing and a read-your-own-writes fallback window) and `SoftDeletable` / `OptimisticLock` conventions in `rume-data`.

## Files touched

- `crates/data/src/persistence/read_write.rs`
- `crates/data/src/persistence/conventions.rs`
- `crates/data/src/persistence/mod.rs`

## Deliverable

1. **`rume-data::persistence::read_write`**:
   - `ReadWriteConfig { ryow_window: Duration, skip_unhealthy_replicas: bool }` (with sensible defaults, e.g. `ryow_window = Duration::from_secs(2)`).
   - `ReadWritePool<B: DatabaseBackend>`:
     - Constructed from `primary: DatabasePool<B>`, `replicas: Vec<DatabasePool<B>>`, and `ReadWriteConfig`.
     - `.primary()` -> `&DatabasePool<B>`, `.replicas()` -> `&[DatabasePool<B>]`.
     - `.read_pool()` -> selects primary if no replicas exist, all replicas are closed/unhealthy, or `record_write()` occurred within `ryow_window`; otherwise round-robins across healthy replicas via `AtomicUsize`.
     - `.record_write()` updates an atomic monotonic timestamp (`Instant` offset from pool creation) so subsequent reads within `ryow_window` route to primary.
     - Convenience methods: `.execute(...)` / `.execute_params(...)` / `.transaction(...)` (route to primary + call `record_write()`), `.fetch_all(...)` / `.fetch_all_as(...)` / `.fetch_optional_as(...)` / `.fetch_one_as(...)` (route via `.read_pool()`), and `.replica_or_primary()` for explicit replica reads.
     - `.status()` / `.health_check(name)` reporting primary + replica health.
2. **`rume-data::persistence::conventions`**:
   - `SoftDeletable` trait (`const DELETED_AT_COLUMN: &'static str = "deleted_at";`, `fn deleted_at(&self) -> Option<&str>`, `fn is_deleted(&self) -> bool`) plus query-builder helpers: `where_not_deleted(select, table_or_col)`, `where_only_deleted(select, table_or_col)`, and `soft_delete_update(table, timestamp)` (`UPDATE <table> SET deleted_at = ? WHERE deleted_at IS NULL`).
   - `OptimisticLock` trait (`const VERSION_COLUMN: &'static str = "version";`, `fn version(&self) -> i64`) plus `with_version_check(update, col, expected_version)` (adds `SET version = version + 1` and `WHERE version = expected_version`) and `execute_cas(pool, built_query, entity, expected_version)` / `execute_cas_tx(...)` returning `Err(PersistenceError::VersionConflict { entity, expected, actual: None })` when affected rows == `0`.

## Constraints honoured

- `crates/data/src/persistence/pool.rs`: reuse `DatabasePool<B>` and `DatabaseHealthCheck` without duplicating pool semaphore/lifecycle logic.
- `crates/foundation/src/error.rs`: `PersistenceError::VersionConflict` converts cleanly into `AppError::Conflict` (`409 Conflict`).
- `#![forbid(unsafe_code)]` and `parking_lot` / lock-free atomics for hot-path routing (`Cargo.toml` rule).

## Named tests (bodies omitted)

- `crates/data/src/persistence/read_write.rs`:
  - `read_write_pool_round_robins_across_healthy_replicas_and_skips_closed` — verifies round-robin distribution and skipping of closed replicas.
  - `read_write_pool_falls_back_to_primary_when_no_replicas` — verifies 0-replica fallback.
  - `read_write_pool_honors_ryow_window_after_write` — verifies writes trigger primary routing for reads within `ryow_window`, and return to replicas after window elapses.
- `crates/data/src/persistence/conventions.rs`:
  - `soft_delete_filters_and_updates_in_sqlite` — verifies `where_not_deleted`, `soft_delete_update`, and `where_only_deleted` against real SQLite.
  - `optimistic_lock_cas_succeeds_and_returns_version_conflict_on_stale_version` — verifies `execute_cas` increments version on match and returns `PersistenceError::VersionConflict` (mapping to `409 Conflict`) on stale version.

## Verification

```bash
cargo test -j 2 -p rume-data --lib --all-features
cargo clippy -j 2 -p rume-data --all-targets --all-features -- -D warnings
```

Expected: all read/write routing, soft-delete, and optimistic-locking tests pass with zero warnings.

## Commit

Only when the user asks:

```text
feat(data): add ReadWritePool replica routing, soft-delete, and optimistic-lock conventions
```

## Done-when

- [ ] `ReadWritePool<B>` routes reads round-robin across healthy replicas, writes to primary, and honors `ryow_window`.
- [ ] `SoftDeletable` and `OptimisticLock` helpers integrate with `sea-query` builder and emit `PersistenceError::VersionConflict`.
- [ ] `cargo test -j 2 -p rume-data --lib --all-features` passes with zero warnings.
