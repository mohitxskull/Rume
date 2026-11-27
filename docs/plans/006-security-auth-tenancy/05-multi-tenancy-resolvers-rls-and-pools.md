# Task 05 — Multi-Tenancy Resolvers, Postgres RLS & `TenantPoolManager`

## Goal

Complete Rume's multi-tenancy stack with `PathPrefixTenantResolver`, `CachedDbTenantResolver` (with negative caching and invalidation), parameterized PostgreSQL RLS transaction hooks (`DatabaseTransaction::set_tenant_context` and `set_tenant_search_path`), and `TenantPoolManager` supporting RLS, schema-per-tenant, and database-per-tenant isolation.

## Files touched

- `crates/data/src/persistence/backend/mod.rs` (implement `validate_tenant_id`, `validate_sql_identifier`, `DatabaseTransaction::set_tenant_context`, `DatabaseTransaction::set_tenant_search_path`)
- `crates/data/src/persistence/tenancy.rs` (implement `TenantIsolationStrategy`, `TenantPoolManager`)
- `crates/data/src/persistence/mod.rs` (re-exports)
- `crates/security/src/tenant/resolver.rs` (implement `PathPrefixTenantResolver`, `TenantStore`, `SqlTenantStore`, `CachedDbTenantResolver`)
- `crates/security/src/tenant/mod.rs` (re-exports including `TenantIsolationStrategy`, `TenantPoolManager`)

## Deliverable

1. **Path Prefix Tenant Resolver (`crates/security/src/tenant/resolver.rs`)**:
   - `PathPrefixTenantResolver`: extracts tenant slug from a configurable path prefix (default `"/t/"`, matching `/t/{slug}` or `/t/{slug}/...`).
   - Validates extracted slug strictly (non-empty, ASCII alphanumeric + `-` / `_`, max 64 chars, rejecting path traversal like `..` or percent-encoded separators).
   - Supports both static registration (`try_add_tenant` / `add_tenant` with conflict checks) and backing by any `TenantStore` (`with_store`).
   - Returns `Ok(None)` when the request path does not start with the configured prefix (allowing non-tenant routes like `/health/live` to pass through when `TenantLayer::optional` is used), and `Err(TenantError::TenantNotFound(slug))` when the prefix matches an unknown slug.
2. **Cached DB-Backed Tenant Resolver (`crates/security/src/tenant/resolver.rs`)**:
   - `TenantStore` trait (async `BoxFuture`): `find_by_id`, `find_by_slug`, `upsert`, `deactivate`.
   - `SqlTenantStore<B: DatabaseBackend>` using `rume_data::sql` queries against `rume_tenants` (filtering `active = true`).
   - `TenantKeyExtractor` enum (`Header(String)`, `Subdomain(String)`, `PathPrefix(String)`) reusing the exact security normalization rules from `HeaderTenantResolver` / `SubdomainTenantResolver` / `PathPrefixTenantResolver`.
   - `CachedDbTenantResolver<S: TenantStore, C: CacheBackend>`:
     - Caches positive lookups under `rume:tenant:<kind>:<key>` with configurable `positive_ttl` (default 5 min).
     - Caches negative lookups (sentinel value) with configurable shorter `negative_ttl` (default 30s) to protect the database from unknown-tenant enumeration floods.
     - Exposes `invalidate(&self, tenant_id: &str, slug: &str)` to evict both ID and slug cache entries on tenant update/deactivation.
3. **PostgreSQL RLS & `search_path` Transaction Hooks (`crates/data/src/persistence/backend/mod.rs`)**:
   - `validate_tenant_id(tenant_id: &str) -> Result<(), PersistenceError>`: rejects empty, > 128 chars, NUL/control characters, or characters outside `[A-Za-z0-9._:-]`.
   - `validate_sql_identifier(ident: &str) -> Result<(), PersistenceError>`: enforces `^[a-zA-Z_][a-zA-Z0-9_]{0,62}$` (PostgreSQL `NAMEDATALEN - 1`).
   - `DatabaseTransaction::set_tenant_context(&self, tenant_id: &str)`:
     - Validates `tenant_id`.
     - If `self.dialect() == "postgres"`: executes `rume_data::sql::PG_SET_TENANT_CONTEXT` (`SELECT set_config('app.current_tenant', $1, true)`) via `query_scalar_params` (bound parameter, transaction-local).
     - Otherwise: fails closed with `PersistenceError::DialectMismatch`.
   - `DatabaseTransaction::set_tenant_search_path(&self, schema: &str)`:
     - Validates `schema` with `validate_sql_identifier`.
     - If `self.dialect() == "postgres"`: executes `rume_data::sql::PG_SET_TENANT_SEARCH_PATH` (`SELECT set_config('search_path', $1, true)`) via `query_scalar_params`.
     - Otherwise: fails closed with `PersistenceError::DialectMismatch`.
4. **`TenantPoolManager<B: DatabaseBackend>` (`crates/data/src/persistence/tenancy.rs`)**:
   - `TenantIsolationStrategy`:
     - `RowLevelSecurity` (shared `DatabasePool<B>`, automatically calls `tx.set_tenant_context(tenant_id)` inside `tenant_transaction`).
     - `SchemaPerTenant { schema_prefix: String }` (shared `DatabasePool<B>`, validates `<prefix><tenant_id>` as a SQL identifier and calls `tx.set_tenant_search_path(&schema)` inside `tenant_transaction`).
     - `DatabasePerTenant` (maintains a thread-safe `RwLock<HashMap<String, DatabasePool<B>>>` of per-tenant pools with `register_tenant_pool`, `remove_tenant_pool`, `pool_for_tenant`, and `tenant_transaction`).
   - `tenant_transaction(&self, tenant_id, f)` executes `f(tx)` inside the appropriate tenant-isolated transaction and commits/rolls back cleanly.

## Constraints honoured

- `crates/security/src/tenant/resolver.rs`: Preserve all existing `HeaderTenantResolver` and `SubdomainTenantResolver` security checks and tests.
- `crates/data/src/persistence/backend/mod.rs`: Fail closed (`DialectMismatch`) if RLS or `search_path` is requested on SQLite/MySQL; never interpolate unvalidated strings into SQL.
- `rules/70-code-rationale.md`: Document why `set_config(..., true)` is used instead of string-interpolated `SET LOCAL`.
- `#![forbid(unsafe_code)]`.

## Named tests (bodies omitted)

- `validate_tenant_id_and_sql_identifier_reject_injection_and_control_chars` (in `rume-data`)
- `database_transaction_set_tenant_context_fails_closed_on_sqlite` (in `rume-data`)
- `tenant_pool_manager_database_per_tenant_routes_and_isolates_sqlite_pools` (in `rume-data`)
- `tenant_pool_manager_schema_per_tenant_rejects_invalid_schema_identifier` (in `rume-data`)
- `path_prefix_tenant_resolver_extracts_slug_and_rejects_traversal_or_unknown` (in `rume-security`)
- `path_prefix_tenant_resolver_returns_none_for_unprefixed_paths` (in `rume-security`)
- `cached_db_tenant_resolver_caches_hits_negative_misses_and_invalidates` (in `rume-security`, using real `SqliteBackend::in_memory()` + `InMemoryCacheBackend`)

## Code-only decisions

- Postgres RLS GUC name: `"app.current_tenant"` — Why: standard PostgreSQL custom GUC namespace convention (`<prefix>.<name>`) compatible with `current_setting('app.current_tenant', true)` in RLS policies.
- `CachedDbTenantResolver` defaults: `positive_ttl = 300s`, `negative_ttl = 30s` — Why: balances DB offload against timely tenant provisioning visibility.

## Verification

```bash
cargo fmt --all --check
cargo clippy -j 2 -p rume-data -p rume-security --all-targets --all-features -- -D warnings
cargo test -j 2 -p rume-data -p rume-security --lib --all-features
```

## Commit

*(only when the user asks or controller announces commit-per-task)*

```
feat(tenancy): add path-prefix/cached-db resolvers, Postgres RLS hooks, and TenantPoolManager
```

## Done-when

- [ ] `DatabaseTransaction::set_tenant_context` and `set_tenant_search_path` validate inputs, use parameterized `set_config(..., true)` on Postgres, and fail closed on non-Postgres dialects.
- [ ] `TenantPoolManager` supports RLS, schema-per-tenant, and database-per-tenant routing with real SQLite multi-pool isolation tests.
- [ ] `PathPrefixTenantResolver` and `CachedDbTenantResolver` (with `SqlTenantStore` + positive/negative caching + invalidation) pass all unit tests.
- [ ] All verification commands pass with zero warnings.
