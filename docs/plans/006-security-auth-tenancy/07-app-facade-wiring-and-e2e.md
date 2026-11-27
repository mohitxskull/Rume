# Task 07 — Facade Wiring (`rume`), End-to-End Integration Tests & Full Verification

## Goal

Wire all Milestone B features and exports cleanly into `rume` (`crates/core`), add end-to-end integration tests exercising HTTP authentication (JWT/JWKS, persistent SQL API keys, SQL sessions, dynamic RBAC/ABAC), multi-tenancy (`PathPrefixTenantResolver`, `CachedDbTenantResolver`, `TenantPoolManager`), and SAML XML-DSig verification, and run the complete workspace verification suite.

## Files touched

- `crates/core/Cargo.toml` (verify feature forwarding: `http-client`, `redis` -> `rume-enterprise/redis`, `saml`, `enterprise`)
- `crates/core/src/lib.rs` (verify all new `rume-security`, `rume-data`, and `rume-enterprise` exports are reachable via `rume::{auth, tenant, persistence, sql, enterprise}`)
- `crates/core/tests/integration/` (new Milestone B end-to-end integration test module wired into `crates/core/tests/integration.rs`)
- `crates/core/tests/containers.rs` (add optional live Postgres RLS `set_tenant_context` + Redis `RedisSamlReplayCache` container tests following the existing testcontainers pattern)

## Deliverable

1. **Facade & Feature Wiring (`crates/core/`)**:
   - Confirm `rume::auth::*` exposes `PasswordHasher`, `PasswordHasherConfig`, `TokenSigner`, `SignedUrl`, `TotpConfig`, `TotpSecret`, `RecoveryCodeSet`, `WebAuthnVerifier`, `JwtVerifier`, `JwtValidationConfig`, `JwkSet`, `JwksFetcher`, `PkceChallenge`, `OAuth2Config`, `OAuth2Client`, `OidcDiscovery`, `OAuth2StateStore`, `InMemoryOAuth2StateStore`, `ApiKeyStore`, `SqlApiKeyStore`, `InMemoryApiKeyStore`, `SqlSessionStore`, `RoleStore`, `SqlRoleStore`, `InMemoryRoleStore`, `RequireDynamicPermission`, `AbacPolicy`, `DynamicPolicyLayer`.
   - Confirm `rume::tenant::*` exposes `PathPrefixTenantResolver`, `TenantStore`, `SqlTenantStore`, `CachedDbTenantResolver`, `TenantIsolationStrategy`, `TenantPoolManager`.
   - Confirm `rume::enterprise::*` (under `feature = "saml"`) exposes `XmlDsigSamlVerifier`, `AsyncSamlReplayCache`, `SyncReplayCacheAdapter`, and (under `feature = "saml"` + `feature = "redis"`) `RedisSamlReplayCache`.
   - Confirm `rume::persistence::*` exposes `TenantIsolationStrategy`, `TenantPoolManager`, `RUME_AUTH_MIGRATION_ID`.
2. **End-to-End Integration Tests (`crates/core/tests/integration/`)**:
   - Full Axum/Rume `TestApp` HTTP flow testing:
     - Argon2id signup/login -> `SqlSessionStore` cookie issuance -> session ID rotation -> authenticated route access.
     - `JwtVerifier` (RS256 + JWKS `kid` rotation) -> `BearerTokenScheme::from_jwt_verifier` -> `AuthLayer` -> `Authenticated` extractor.
     - `SqlApiKeyStore` -> `ApiKeyScheme::with_store` -> `DynamicPolicyLayer` with `SqlRoleStore` live permission revocation (200 before revoke, 403 after revoke).
     - `PathPrefixTenantResolver` + `CachedDbTenantResolver` (SQLite + `InMemoryCacheBackend`) + `TenantLayer` + `TenantMembershipLayer` enforcing cross-tenant 403 isolation.
     - Real RSA-SHA256 signed SAML 2.0 assertion verified end-to-end through `XmlDsigSamlVerifier` + `SamlParser::parse_signed` + `validate_browser_sso_async`.
3. **Container Integration Tests (`crates/core/tests/containers.rs`)**:
   - Real PostgreSQL RLS test verifying `tx.set_tenant_context("tenant-a")` enforces a Postgres `CREATE POLICY` so tenant A only sees tenant A rows and cannot see tenant B rows, and transaction-local isolation resets after commit.
   - Real Redis `RedisSamlReplayCache` test verifying first assertion succeeds (`Ok(true)`), replay is rejected (`Ok(false)`), and TTL is set on the Redis key.

## Constraints honoured

- `rules/40-verification.md`: Run the full workspace check — fmt, clippy (`-D warnings`), lib tests, integration tests, doc tests, and feature-matrix check (default features vs `--all-features`).
- `rules/testing.md`: Integration tests use `TestApp` / real SQLite / real containers (never mocks for DB or core engine), and container tests report explicit `[skipping …]` if Docker is unavailable.
- `rules/80-quality-bar.md`: Zero warnings across all targets and feature combinations.

## Named tests (bodies omitted)

- `e2e_jwt_jwks_bearer_auth_and_kid_rotation_over_http`
- `e2e_sql_api_key_and_dynamic_rbac_live_revocation_over_http`
- `e2e_sql_session_login_rotation_and_logout_over_http`
- `e2e_path_prefix_and_cached_db_tenant_resolution_with_membership_isolation`
- `e2e_saml_xmldsig_rsa_sha256_and_async_replay_protection`
- `container_postgres_rls_enforces_tenant_isolation_via_set_tenant_context` (in `containers.rs`)
- `container_redis_saml_replay_cache_blocks_replay_and_sets_ttl` (in `containers.rs`)

## Verification

```bash
cargo fmt --all --check
cargo deny check licenses
cargo run -j 2 -p rume-cli -- sql:check crates/data/sql
cargo clippy -j 2 --workspace --all-targets --all-features -- -D warnings
cargo check -j 2 -p rume --no-default-features
cargo check -j 2 -p rume --features "sqlite,saml"
cargo test -j 2 --workspace --lib --all-features
cargo test -j 2 -p rume --test integration --all-features
cargo test -j 2 --workspace --doc --all-features
```

## Commit

*(only when the user asks or controller announces commit-per-task)*

```
feat(core): wire Milestone B security/auth/tenancy facade and add e2e tests
```

## Done-when

- [ ] All Milestone B types, traits, and features are cleanly re-exported through `rume`.
- [ ] All 5 end-to-end HTTP/SQL/SAML integration tests pass in `cargo test -j 2 -p rume --test integration --all-features`.
- [ ] Postgres RLS and Redis SAML replay container tests compile cleanly and pass (when Docker is running).
- [ ] All verification commands pass with zero warnings.
