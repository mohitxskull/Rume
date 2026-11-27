# Task 04 — Persistent Auth Stores (`SqlApiKeyStore`, `SqlSessionStore`) & Dynamic RBAC/ABAC

## Goal

Implement persistent SQL-backed `ApiKeyStore` (`SqlApiKeyStore`, wired into `ApiKeyScheme`), `SqlSessionStore` (implementing `SessionStore` with atomic `regenerate_id`), and tenant-scoped dynamic RBAC/ABAC (`RoleStore`, `SqlRoleStore`, `RequireDynamicPermission`, `AbacPolicy`, `DynamicPolicyLayer`) across SQLite, PostgreSQL, and MySQL.

## Files touched

- `crates/security/src/auth/api_key.rs`
- `crates/security/src/auth/sql_session.rs`
- `crates/security/src/auth/rbac.rs`
- `crates/security/src/auth/schemes.rs` (wire `ApiKeyScheme::with_store(...)`)
- `crates/security/src/auth/mod.rs` (exports)
- `crates/security/Cargo.toml` (add `rume-data` feature `sqlite` in `[dev-dependencies]` so unit tests run against real in-memory SQLite!)

## Deliverable

1. **API Key Management (`crates/security/src/auth/api_key.rs` + `schemes.rs`)**:
   - `ApiKeyRecord`: `id`, `prefix` (first 12 chars, e.g. `rk_live_abcd`), `key_hash: [u8; 32]` (SHA-256), `user_id`, `tenant_id: Option<String>`, `name`, `scopes: Vec<String>`, `expires_at: Option<u64>`, `revoked_at: Option<u64>`, `created_at: u64`, `last_used_at: Option<u64>`.
   - `IssuedApiKey`: `{ record: ApiKeyRecord, plaintext: SecretString }` (returned once at creation/rotation).
   - `ApiKeyStore` trait (async `BoxFuture`): `create`, `find_by_prefix`, `verify_key(&self, raw_key: &str, now_epoch_sec: u64) -> Result<Option<ApiKeyRecord>, AuthError>` (default or impl doing prefix lookup + `subtle::ConstantTimeEq` hash check + expiry + revocation check), `revoke`, `rotate(old_id, new_ttl_secs)` (atomically creates new key with same user/tenant/scopes and revokes old key inside a transaction), `list_for_user`, `touch_last_used`.
   - `InMemoryApiKeyStore` and `SqlApiKeyStore<B: DatabaseBackend>` using `rume_data::sql` dialect-dispatched queries (`postgres` / `mysql` / `sqlite`).
   - `ApiKeyScheme::with_store(store: impl ApiKeyStore)` in `crates/security/src/auth/schemes.rs`: preserves existing static `ApiKeyScheme::new().add_key(...)` while supporting store-backed lookup that maps `ApiKeyRecord` -> `AuthUser` (setting `user_id`, `tenant_id`, and `scopes` as `permissions`).
2. **SQL Session Store (`crates/security/src/auth/sql_session.rs`)**:
   - `SqlSessionStore<B: DatabaseBackend>` implementing `SessionStore` (`load`, `save`, `destroy`, `regenerate_id`) plus `prune_expired(now_epoch_sec) -> Result<u64, AuthError>`.
   - Uses `rume_data::sql` dialect-dispatched queries.
   - `regenerate_id`: executes inside `backend.begin_immediate_transaction()` (SQLite) / `begin_transaction()` (Postgres/MySQL): loads existing row, rejects missing or expired rows (`AuthError::InvalidCredentials`), deletes old ID (verifying `rows_affected == 1` so concurrent logout/destroy cannot resurrect a session), inserts new UUID v4 ID with identical user/tenant/roles/permissions/data/expires_at, and commits atomically.
3. **Dynamic Persistent RBAC & ABAC (`crates/security/src/auth/rbac.rs`)**:
   - `Role`: `id`, `tenant_id: Option<String>`, `name`, `description`, `permissions: Vec<String>`.
   - `RoleStore` trait (async `BoxFuture`): `create_role`, `delete_role`, `grant_permission`, `revoke_permission`, `assign_role(user_id, tenant_id, role_id)`, `unassign_role`, `effective_permissions(user_id, tenant_id) -> Result<BTreeSet<String>, AuthError>`, `user_roles(user_id, tenant_id)`.
   - `InMemoryRoleStore` and `SqlRoleStore<B: DatabaseBackend>` enforcing strict tenant scoping (roles assigned in tenant A never bleed into tenant B), mapping `tenant_id: Option<String>` (`None` <-> `""`) at the SQL boundary against `tenant_id NOT NULL DEFAULT ''` so composite primary keys, `UNIQUE` constraints, and parameterized `WHERE` queries behave identically across SQLite, Postgres, and MySQL.
   - `AbacContext`: `{ user: AuthUser, tenant_id: Option<String>, action: String, resource_type: String, resource_id: Option<String>, attributes: BTreeMap<String, String> }`.
   - `AbacPolicy` trait: `evaluate(&self, ctx: &AbacContext) -> BoxFuture<'_, Result<bool, AuthError>>`, plus combinators `AllOfPolicy`, `AnyOfPolicy`, and `RequireDynamicPermission<S: RoleStore>` (checks live `RoleStore::effective_permissions` for `(user.id, resolved_tenant_or_user_tenant)`).
   - `DynamicPolicyLayer<P: AbacPolicy>`: Tower middleware layer extracting `AuthUser` and optional `Tenant` from request extensions, evaluating the async policy, and returning `AppError::Forbidden` (403) on denial or `AppError::Unauthorized` (401) if unauthenticated.

## Constraints honoured

- `crates/security/src/auth/session.rs:104-110`: `SqlSessionStore::regenerate_id` MUST be atomic and prevent logout resurrection (TOCTOU).
- `crates/security/src/auth/schemes.rs`: Static `ApiKeyScheme::new().add_key(...)` tests must remain passing unchanged.
- `crates/data/src/sql.rs`: All SQL queries come from `rume_data::sql` constants (no inline raw SQL strings in `crates/security`).
- `rules/testing.md`: Test `SqlApiKeyStore`, `SqlSessionStore`, and `SqlRoleStore` against real `SqliteBackend::in_memory()` (with `Migrator::new().with_rume_auth("sqlite")`), never mocks.

## Named tests (bodies omitted)

- `sql_api_key_store_create_verify_revoke_and_expiry_on_sqlite`
- `sql_api_key_store_rotate_revokes_old_key_and_issues_new_key_atomically`
- `api_key_scheme_with_store_authenticates_and_maps_scopes_and_tenant`
- `sql_session_store_save_load_destroy_and_prune_expired_on_sqlite`
- `sql_session_store_regenerate_id_rotates_atomically_and_rejects_expired_or_missing`
- `sql_role_store_enforces_tenant_isolation_for_roles_and_permissions`
- `sql_role_store_global_roles_deduplicate_and_query_cleanly_via_empty_sentinel`
- `require_dynamic_permission_and_dynamic_policy_layer_enforce_live_revocation`
- `abac_policy_combinators_all_of_and_any_of_evaluate_attributes`

## Code-only decisions

- API key format: `rk_<12-char-random-prefix>_<32-byte-base64url-secret>` — Why: unambiguous prefix lookup in SQL without exposing key material, while the full string is SHA-256 hashed for constant-time verification.
- Global role sentinel `tenant_id = ""` in `SqlRoleStore` — Why: Postgres/MySQL forbid `NULL` in composite PKs and SQL `NULL != NULL` breaks `WHERE tenant_id = $2` for global roles; `validate_tenant_id` rejects `""` so no real tenant ID can collide.

## Verification

```bash
cargo fmt --all --check
cargo clippy -j 2 -p rume-security --all-targets --all-features -- -D warnings
cargo test -j 2 -p rume-security --lib --all-features
```

## Commit

*(only when the user asks or controller announces commit-per-task)*

```
feat(security): add SqlApiKeyStore, SqlSessionStore, and dynamic RBAC/ABAC
```

## Done-when

- [ ] `SqlApiKeyStore` creates, verifies (constant-time), rotates, revokes, and expires keys on SQLite, and `ApiKeyScheme::with_store` integrates cleanly into `AuthLayer`.
- [ ] `SqlSessionStore` implements `SessionStore` with atomic `regenerate_id` and passes all lifecycle/expiry tests on SQLite.
- [ ] `SqlRoleStore`, `RequireDynamicPermission`, `AbacPolicy`, and `DynamicPolicyLayer` enforce live tenant-scoped permissions on SQLite.
- [ ] Zero inline SQL strings in `crates/security` (all queries imported from `rume_data::sql`).
- [ ] All verification commands pass with zero warnings.
