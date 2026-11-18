# 16-rc11-macro-and-cli-db.md

## Goal

Fix RC-11 (macro + CLI db command), three defects:

1. `crates/macro/src/lib.rs` `extract_migration_version` (~line 337): uses `.any(|c| c.is_ascii_digit())` to decide whether a `_`-separated part is a version segment, so `001a_add_users` is treated as version `001a` instead of requiring every char to be a digit. Fix: use `.all(|c| c.is_ascii_digit())` so only all-digit parts count as version segments.
2. `crates/cli/src/commands/db/mod.rs` `resolve_db_url` (~lines 150-174): bypasses Rume's env/config system — reads `DATABASE_URL` directly, calls `dotenvy::dotenv()`, and parses `config/app.toml` with raw `toml::Value` instead of `crate::env::load` / `crate::config`. Fix: route URL resolution through Rume's `env`/`config` loading.
3. `connect_backend` (~lines 182-198): rejects `mariadb://` URLs (falls through to the SQLite backend). Fix: accept `mariadb://` as a MySQL-compatible backend (route to `MysqlBackend` or a MariaDB path).

## Files touched

- `crates/macro/src/lib.rs` — `extract_migration_version` (~line 337).
- `crates/cli/src/commands/db/mod.rs` — `resolve_db_url` (~lines 150-174); `connect_backend` (~lines 182-198) to accept `mariadb://`.

## Constraints honoured

- Root cause, not symptom: version detection is digit-exact; URL resolution uses Rume env/config; MariaDB is a recognized backend.
- No new dependencies, no `unsafe`.
- Record `Why:` comments: `.all()` for digit-exact versions; env/config routing for single source of truth; MariaDB treated as MySQL-compatible.
- Confirm `extract_migration_version` callers/tests aren't broken by stricter digit matching.

## Done when (checkable by a command)

- `cargo test -p rume-macro` passes (migration version tests).
- `cargo build -p rume-cli` passes.
- `cargo clippy -p rume-macro -p rume-cli --all-targets` and `cargo fmt --all --check` clean.
- `extract_migration_version("001_add_users") == "001"`; `extract_migration_version("001a_add_users")` does NOT treat `001a` as the version prefix.
- `connect_backend("mariadb://...")` returns a `BackendHandle::Mysql` (not SQLite).

## Verification (fails-before test idea)

Tests:
1. **Migration version:** `extract_migration_version("001_add_users")` → `"001"`; `extract_migration_version("001a_add_users")` → **before fix** returns `"001a"` (treats `001a` as version because `.any` digit) → test fails; **after fix** digit-exact → test passes.
2. **MariaDB:** `connect_backend("mariadb://user:pass@host:3306/db")` → **before fix** falls to SQLite backend → test fails; **after fix** returns `BackendHandle::Mysql` → passes.
3. **resolve_db_url uses env/config:** (structural) assert resolution goes through `crate::env`/`crate::config` rather than raw `dotenvy`/`toml::Value` — grep confirms `dotenvy::dotenv()` and direct `toml::Value` parse removed from `resolve_db_url`.
