# Task 09 — Self-Contained Crate Packaging & `rume-crash-harness` Extraction (`RC-05`)

## Goal

Eliminate all 22 cross-crate `include_str!("../../../data/...")` paths so every publishable workspace crate packages cleanly with `cargo package`, extract `rume-crash-harness` and its `crash_*.sql` assets into a dedicated `publish = false` workspace crate `crates/crash-harness`, and demote `tokio`, `sqlx`, and `uuid` in `crates/core/Cargo.toml` to `[dev-dependencies]`.

## Files touched

- `Cargo.toml` (`[workspace.members]`: add `"crates/crash-harness"`)
- `crates/data/src/persistence/sql.rs` (new module exporting shared core/audit/isolation SQL constants)
- `crates/data/src/persistence/mod.rs` (add `pub mod sql;`)
- `crates/data/src/persistence/sql/postgres/crash_{orders_table,side_effects_insert,side_effects_table}.sql` (move out to `crates/crash-harness/sql/`)
- `crates/data/src/persistence/sql/postgres/jobs_{ack,claim,dead_letter,dead_letter_count,idempotency_check,insert,list_dead_letters,nack,pending_count,purge,reap,renew_lease,replay_dead_letter}.sql` (move out to `crates/messaging/src/jobs/sql/postgres/`)
- `crates/messaging/src/jobs/sql/postgres/jobs_defer.sql` (new, externalized from inline SQL in `PostgresJobQueue::defer`)
- `crates/messaging/src/jobs/postgres.rs` (include crate-local `sql/postgres/*.sql`, reference `rume_data::persistence::sql::PG_JOBS_SCHEMA`)
- `crates/messaging/src/outbox/mysql.rs` (use `rume_data::persistence::sql::MYSQL_SET_ISOLATION_LEVEL`)
- `crates/security/src/security/audit.rs` (use `rume_data::persistence::sql::{PG_AUDIT_SCHEMA, MYSQL_AUDIT_SCHEMA, SQLITE_AUDIT_SCHEMA, AUDIT_COUNT_SQL}`)
- `crates/crash-harness/Cargo.toml` (new, `publish = false`)
- `crates/crash-harness/src/main.rs` (moved from `crates/core/src/bin/crash_harness.rs`, `#![forbid(unsafe_code)]`, local `include_str!("../sql/...")`)
- `crates/core/src/bin/crash_harness.rs` (remove)
- `crates/core/Cargo.toml` (remove `[[bin]] name = "rume-crash-harness"`, demote `tokio`/`sqlx`/`uuid` to `[dev-dependencies]`, clean feature `dep:sqlx` refs)
- `crates/core/tests/containers/crash_recovery.rs` (`crash_harness_binary()` locates/builds `rume-crash-harness`)

## Deliverable

1. **`rume_data::persistence::sql` (`crates/data/src/persistence/sql.rs`):** Expose `PG_JOBS_SCHEMA`, `PG_AUDIT_SCHEMA`, `MYSQL_AUDIT_SCHEMA`, `SQLITE_AUDIT_SCHEMA`, `AUDIT_COUNT_SQL`, and `MYSQL_SET_ISOLATION_LEVEL` via crate-local `include_str!("sql/...")` constants (keeping `jobs_schema.sql` and `audit_schema.sql` in `crates/data` because `Migrator::with_rume_core` requires them!).
2. **`rume-messaging` & `rume-security` self-contained SQL:**
   - Move the 13 `jobs_*.sql` operation queries (excluding `jobs_schema.sql`) from `crates/data/src/persistence/sql/postgres/` to `crates/messaging/src/jobs/sql/postgres/`, plus new `jobs_defer.sql` externalizing the inline `UPDATE rume_jobs ...` from `PostgresJobQueue::defer` (`postgres.rs:324-336`).
   - Point `PostgresJobQueue::SCHEMA` to `rume_data::persistence::sql::PG_JOBS_SCHEMA` and `crates/messaging/src/outbox/mysql.rs` to `rume_data::persistence::sql::MYSQL_SET_ISOLATION_LEVEL`.
   - Point `SqlAuditLogger` constants in `crates/security/src/security/audit.rs` to `rume_data::persistence::sql::{PG_AUDIT_SCHEMA, MYSQL_AUDIT_SCHEMA, SQLITE_AUDIT_SCHEMA, AUDIT_COUNT_SQL}`.
3. **`crates/crash-harness` extraction & `crates/core/Cargo.toml` cleanup:**
   - Create `crates/crash-harness` (`publish = false`, `#![forbid(unsafe_code)]`) depending on `rume = { path = "../core", features = ["postgres", "redis"] }`, `tokio`, `sqlx` (`postgres`), and `uuid`, housing `src/main.rs` and `sql/crash_*.sql`.
   - Remove `[[bin]] name = "rume-crash-harness"` from `crates/core/Cargo.toml` and delete `crates/core/src/bin/crash_harness.rs`.
   - Demote `tokio`, `sqlx`, and `uuid` in `crates/core/Cargo.toml` from `[dependencies]` to `[dev-dependencies]` (removing `"dep:sqlx"`, `"sqlx/postgres"`, `"sqlx/sqlite"`, `"sqlx/mysql"` from `[features]` because `rume-data/*` and `rume-messaging/*` enable the needed `sqlx/*` features via Cargo feature unification when tests enable `rume`'s backend features).
   - Update `crates/core/tests/containers/crash_recovery.rs::crash_harness_binary()` to check `CARGO_BIN_EXE_rume-crash-harness` first and otherwise locate the active target profile directory (via `std::env::current_exe()`'s grandparent `target/<profile>`) and ensure `rume-crash-harness` is compiled once via `std::sync::OnceLock` running `std::env::var_os("CARGO")` (defaulting to `"cargo"`) with `.current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))` (`["build", "-p", "rume-crash-harness", "--bin", "rume-crash-harness"]`, plus `"--release"` if `not(debug_assertions)`), asserting exit status (printing captured `stderr` on failure) and `bin_path.is_file()` before returning.

## Constraints honoured

- `rules/70-code-rationale.md`:
  - `Why:` comment in `crates/data/src/persistence/sql.rs` explaining why core/audit/isolation DDL constants are exported from `rume-data` (single owner alongside `Migrator::with_rume_core`) while messaging-only operational queries live inside `rume-messaging` so every publishable crate packages standalone.
  - `Why:` comment on `crash_harness_binary()` in `crash_recovery.rs` explaining why `OnceLock` invokes `cargo build -p rume-crash-harness` after extraction out of `crates/core`.
- `.github/workflows/ci.yml:91-106` (isolated feature matrix `sqlite,testing`, `postgres,testing`, `mysql,testing`, `redis,testing` on `rume`).
- `crates/core/src/lib.rs` & `crates/core/src/prelude.rs` (`rume` library API unchanged).

## Named tests (bodies omitted)

- `workspace_crates_have_no_cross_crate_include_str_and_package_list_succeeds` (verification inspection + `cargo package --list --allow-dirty`) — asserts zero `include_str!("../../../data/` paths remain in `crates/` and every publishable crate lists only its own files.
- `postgres_job_queue_defer_uses_externalized_sql_constant` (`crates/messaging/src/jobs/postgres.rs` `#[cfg(test)]`) — verifies `JOBS_DEFER_SQL` is non-empty and sets `status = 'pending'`, `attempt = GREATEST(attempt - 1, 0)`, and `lease_expires_at` guard.

## Verification

```bash
! rg -n 'include_str!\("\.\./\.\./\.\./data/' crates/
for pkg in rume-foundation rume-observability rume-data rume-messaging rume-security rume-http rume-enterprise rume-app rume-testing rume rume-macros rume-cli; do
  cargo package -p "$pkg" --list --allow-dirty >/dev/null || exit 1
done
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -j 2 -- -D warnings
cargo check -p rume --no-default-features --features sqlite,testing -j 2
cargo check -p rume --no-default-features --features postgres,testing -j 2
cargo check -p rume --no-default-features --features mysql,testing -j 2
cargo check -p rume --no-default-features --features redis,testing -j 2
cargo check -p rume-crash-harness -j 2
cargo test -p rume --test integration --all-features -j 2
```

(And with Docker running: `cargo test -p rume --test containers --all-features crash_recovery -j 2`.)

Expected: zero cross-crate `include_str!` matches, all 12 `cargo package --list` checks pass, all isolated feature builds succeed, and tests pass with 0 warnings.

## Commit

Only when the user asks:

```text
refactor(workspace): make crates self-contained for packaging and extract rume-crash-harness
```

## Done-when

- [ ] `! rg -n 'include_str!\("\.\./\.\./\.\./data/' crates/` exits 0.
- [ ] `cargo package -p <pkg> --list --allow-dirty` succeeds for all 12 publishable crates.
- [ ] `crates/core/Cargo.toml` has no `[[bin]]` section and places `tokio`, `sqlx`, `uuid` only in `[dev-dependencies]`.
- [ ] `crates/crash-harness/Cargo.toml` has `publish = false` and `crates/crash-harness/src/main.rs` has `#![forbid(unsafe_code)]`.
- [ ] All verification commands exit 0 with 0 warnings.
