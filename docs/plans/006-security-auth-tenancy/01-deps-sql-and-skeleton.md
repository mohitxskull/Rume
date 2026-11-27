# Task 01 — Workspace Dependencies, SQL Assets & Compiling Skeleton

## Goal

Pin all new Milestone B dependencies in root `Cargo.toml` (verified against `deny.toml`), add all SQL DDL/query assets and `Migrator::with_rume_auth` in `rume-data`, and scaffold compiling `todo!()` module/export skeletons across `rume-data`, `rume-security`, and `rume-enterprise` so tasks `02`–`06` implement against real compiler-checked types.

## Files touched

- `Cargo.toml` (root `[workspace.dependencies]`)
- `crates/data/Cargo.toml`
- `crates/data/src/sql.rs`
- `crates/data/sql/{sqlite,postgres,mysql}/` (new DDL & query files for `rume_sessions`, `rume_api_keys`, `rume_roles`, `rume_role_permissions`, `rume_user_roles`, `rume_tenants`)
- `crates/data/src/persistence/backend/mod.rs` (add `validate_tenant_id`, `validate_sql_identifier`, and default `DatabaseTransaction::{set_tenant_context, set_tenant_search_path}` stubs)
- `crates/data/src/persistence/tenancy.rs` (new skeleton)
- `crates/data/src/persistence/mod.rs`
- `crates/data/src/persistence/migration/{mod.rs, preflight.rs, runner.rs}` (add `RUME_AUTH_MIGRATION_ID` and `Migrator::with_rume_auth`)
- `crates/security/Cargo.toml`
- `crates/security/src/auth/{mod.rs, password.rs, tokens.rs, mfa.rs, jwt.rs, oauth2.rs, api_key.rs, sql_session.rs, rbac.rs}` (new skeleton modules + exports)
- `crates/security/src/tenant/{mod.rs, resolver.rs}` (skeleton stubs for `PathPrefixTenantResolver`, `TenantStore`, `CachedDbTenantResolver`)
- `crates/enterprise/Cargo.toml` (add `bergshamra` under `saml`, optional `deadpool-redis` + `redis` under `redis` feature)
- `crates/enterprise/src/saml/{mod.rs, xmldsig.rs, redis_replay.rs}` (new skeleton modules + exports)
- `crates/enterprise/src/enterprise/mod.rs`
- `crates/core/Cargo.toml` (forward `rume-enterprise/redis` in `redis` feature and add `http-client = ["rume-security/http-client"]`)

## Deliverable

1. **Workspace dependency pins** in root `Cargo.toml` (all concrete `=x.y.z`, matching `Cargo.lock` where already present):
   - `argon2 = "=0.6.0"`
   - `base64 = "=0.23.1"`
   - `hex = "=0.4.3"`
   - `sha1 = "=0.11.0"`
   - `sha2 = "=0.11.1"`
   - `hmac = "=0.13.0"`
   - `rand = "=0.10.3"`
   - `rsa = { version = "=0.10.0", features = ["pkcs1v15"] }`
   - `p256 = "=0.14.0"`
   - `p384 = "=0.14.0"`
   - `ecdsa = "=0.17.0"`
   - `signature = "=3.0.0"`
   - `zeroize = "=1.9.1"`
   - `reqwest = { version = "=0.13.5", default-features = false, features = ["json", "rustls", "http2"] }`
   - `bergshamra = { version = "=0.12.0", default-features = false, features = ["rustcrypto"] }`
   - Update existing `saml-rs` pin in root `Cargo.toml` to `saml-rs = { version = "=0.8.0", default-features = false }` (Rume only consumes `saml-rs` namespace constants; disabling default features prevents Cargo feature unification from silently re-enabling `bergshamra/legacy-algorithms` / SHA-1).
   - In `crates/security/Cargo.toml`, migrate inline `sha2`/`hmac` dependencies to `.workspace = true` and add `subtle.workspace = true`, `secrecy.workspace = true`, `url.workspace = true`, and the newly pinned crypto/encoding workspace dependencies (`reqwest` gated behind optional `http-client` feature).
2. **SQL assets in `crates/data/sql/` + `crates/data/src/sql.rs`**:
   - Per-dialect schema DDL (`sqlite`, `postgres`, `mysql`) for:
     - `rume_sessions` (`id`, `user_id`, `tenant_id`, `roles_json`, `permissions_json`, `data_json`, `expires_at`, `created_at`)
     - `rume_api_keys` (`id`, `prefix`, `key_hash`, `user_id`, `tenant_id`, `name`, `scopes_json`, `expires_at`, `revoked_at`, `created_at`, `last_used_at`)
     - `rume_roles` (`id`, `tenant_id` non-null `''` sentinel for global scope, `name`, `description`)
     - `rume_role_permissions` (`role_id`, `permission`)
     - `rume_user_roles` (`user_id`, `tenant_id` non-null `''` sentinel for global scope, `role_id`)
     - `rume_tenants` (`id`, `slug`, `metadata_json`, `active`, `created_at`)
   - All MySQL DDL PK and indexed string columns (`id`, `prefix`, `slug`, `user_id`, `tenant_id`, `role_id`, `permission`, `name`) MUST use bounded `VARCHAR(...)` (never `TEXT`) to satisfy InnoDB key-length rules.
   - PostgreSQL RLS/schema SQL assets: `crates/data/sql/postgres/pg_set_tenant_context.sql` (`SELECT set_config('app.current_tenant', $1, true)`) and `crates/data/sql/postgres/pg_set_tenant_search_path.sql` (`SELECT set_config('search_path', $1, true)`), exported in `crates/data/src/sql.rs`.
   - Parameterized CRUD SQL assets for `SqlSessionStore`, `SqlApiKeyStore`, `SqlRoleStore`, and `SqlTenantStore` (using `$1..$N` for Postgres and `?` for SQLite/MySQL).
   - `Migrator::with_rume_auth(dialect)` registering `RUME_AUTH_MIGRATION_ID = "20261126_001_rume_auth_schemas"` with reversible `down_sql`.
3. **Compiling skeletons** across `rume-data`, `rume-security`, and `rume-enterprise` exporting all public types, traits, and method signatures with `todo!()` bodies and `#[allow(dead_code, unused_variables)]` only where needed during the skeleton phase (removed as each task implements its module).

## Constraints honoured

- `rules/10-tech-stack.md`: Every dependency version is a concrete `=x.y.z` pin.
- `deny.toml`: All new dependencies must use allowed licenses (`MIT`, `Apache-2.0`, `BSD-2-Clause`, etc.).
- `crates/data/src/sql.rs:3-6`: All SQL files live under `crates/data/sql/` and are re-exported from `rume_data::sql`.
- `crates/data/src/persistence/migration/runner.rs:115-120`: Never modify `RUME_CORE_MIGRATION_ID` or `RUME_AUDIT_MIGRATION_ID` SQL or checksums.
- `#![forbid(unsafe_code)]` across all library crates.
- Strict 7-layer DAG: `rume-data` (L2) does not depend on `rume-security` (L3) or `rume-enterprise` (L3).

## Named tests (bodies omitted)

- `rume_auth_migration_applies_and_rolls_back_on_sqlite` (in `crates/data/src/persistence/migration/tests.rs`): verifies `Migrator::new().with_rume_auth("sqlite")` creates all 6 auth/tenancy tables idempotently and rolls back cleanly without touching `rume_core` or `rume_audit` migrations.
- `rume_core_and_audit_migration_checksums_remain_unchanged` (in `crates/data/src/persistence/migration/tests.rs`): verifies existing `with_rume_core` migration IDs and behavior are completely preserved.

## Code-only decisions

- `RUME_AUTH_MIGRATION_ID = "20261126_001_rume_auth_schemas"` — Why: append-only migration ID separate from `20261104_001_rume_core_schemas` and `20261104_002_rume_audit_schema` so existing deployments never hit `MigrationChecksumMismatch`.
- `rume_roles.tenant_id` and `rume_user_roles.tenant_id` use `TEXT NOT NULL DEFAULT ''` (SQLite/Postgres) and `VARCHAR(128) NOT NULL DEFAULT ''` (MySQL) — Why: Postgres/MySQL forbid `NULL` in composite `PRIMARY KEY (user_id, tenant_id, role_id)`, SQL `NULL != NULL` breaks `UNIQUE` deduplication and `WHERE tenant_id = $2` lookups for global roles, and `validate_tenant_id` rejects `""` so `""` cannot collide with any real tenant ID.
- `rume_api_keys` indexes `prefix` (non-unique or unique 8–16 char public lookup prefix) and stores SHA-256 hex/bytes of the full key in `key_hash` — Why: avoids full-table scans while keeping verification constant-time and never storing plaintext secrets.

## Verification

```bash
cargo fmt --all --check
cargo deny check licenses
cargo run -j 2 -p rume-cli -- sql:check crates/data/sql
cargo check -j 2 --workspace --all-targets --all-features
cargo test -j 2 -p rume-data --lib --all-features
```

## Commit

*(only when the user asks or controller announces commit-per-task)*

```
chore(security): pin Milestone B deps, add auth SQL assets, and scaffold skeleton
```

## Done-when

- [ ] All Milestone B dependencies are pinned with `=x.y.z` in root `Cargo.toml` (including `saml-rs` with `default-features = false`) and pass `cargo deny check licenses`.
- [ ] All SQL DDL and CRUD files exist under `crates/data/sql/{sqlite,postgres,mysql}/`, are exported in `crates/data/src/sql.rs`, and pass `rume sql:check crates/data/sql`.
- [ ] `Migrator::with_rume_auth` passes `rume_auth_migration_applies_and_rolls_back_on_sqlite`.
- [ ] `cargo check -j 2 --workspace --all-targets --all-features` succeeds with zero errors.
