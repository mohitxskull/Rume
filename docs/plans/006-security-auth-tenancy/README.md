# Plan 006 — Milestone B: Security, Auth & Multi-Tenancy

## Goal

Close all five Milestone B security, authentication, and multi-tenancy gaps cleanly and idiomatically, executed **strictly sequentially on `main`** (one task at a time):

1. **Password Hashing, Signed Tokens & MFA (`rume-security`)** — Argon2id password hashing/verification (`PasswordHasher` using `argon2 = "=0.6.0"` with OWASP 2024 defaults, `SecretString`, constant-time verification, and `spawn_blocking` async wrappers); expiring HMAC-SHA256 signed tokens & signed URLs (`TokenSigner`, `SignedUrl`) with constant-time verification and replay-safe expiry; RFC 6238 TOTP MFA (`TotpConfig`, `TotpSecret`, `otpauth://` URI generation, SHA-1/SHA-256/SHA-512, drift window, hashed recovery codes) plus a pluggable `WebAuthnVerifier` trait hook.
2. **JWT / JWKS & OAuth2 / OIDC (`rume-security`)** — Built-in `JwtVerifier` supporting HS256/HS384/HS512, RS256/RS384/RS512, and ES256/ES384 with JWKS key-set parsing, `kid` lookup, TTL-cached auto-refresh via a hermetic `JwksFetcher` trait (plus optional `ReqwestJwksFetcher` under the `http-client` feature), and `BearerTokenScheme::from_jwt_verifier(...)` integration; OAuth2/OIDC authorization-code + PKCE (S256) + state/nonce validation (`OAuth2Config`, `PkceChallenge`, `OidcDiscovery`, `OAuth2StateStore` + `InMemoryOAuth2StateStore`).
3. **Persistent Auth Stores & Dynamic RBAC / ABAC (`rume-security` + `rume-data`)** — `ApiKeyStore` trait + `SqlApiKeyStore` (prefix lookup, SHA-256 secret hash, scopes, expiry, rotation, revocation) wired into `ApiKeyScheme::with_store(...)`; `SqlSessionStore` implementing `SessionStore` with atomic `regenerate_id` across SQLite/Postgres/MySQL; dynamic persistent RBAC/ABAC (`RoleStore` trait + `SqlRoleStore`, tenant-scoped role & permission assignments, `RequireDynamicPermission`, `AbacPolicy`, `DynamicPolicyLayer`).
4. **Multi-Tenancy Completeness (`rume-security` + `rume-data`)** — `PathPrefixTenantResolver` (`/t/{slug}/...`) and `CachedDbTenantResolver` (DB-backed via `TenantStore` + `CacheBackend` with negative-caching and explicit invalidation); PostgreSQL RLS transaction hooks (`DatabaseTransaction::set_tenant_context` and `DatabaseTransaction::set_tenant_search_path` using parameterized `set_config(..., true)` and validated identifiers); `TenantPoolManager` supporting Row-Level Security, schema-per-tenant, and database-per-tenant pool routing.
5. **Production SAML 2.0 Verification (`rume-enterprise`)** — Real cryptographic XML-DSig verifier (`XmlDsigSamlVerifier` supporting enveloped RSA-SHA256 and ECDSA-SHA256 signatures, Exclusive C14N canonicalization, X.509 certificate trust pinning, and anti-wrapping reference binding) and `RedisSamlReplayCache` (atomic Redis `SET NX EX` via `AsyncSamlReplayCache` + `SamlParser::validate_browser_sso_async`).

### Out of scope

- Milestone C (Cron scheduler, `RumeClient`, `SqliteJobQueue`/`MysqlJobQueue`, `SqlSagaStore`)
- Milestone D (WebSockets/SSE `RealtimeHub`, Multipart/ObjectStorage, `#[derive(FromRow)]`, CLI `rume openapi`/`rume task`)
- Full WebAuthn/FIDO2 ceremony protocol state machine (covered by the pluggable `WebAuthnVerifier` trait hook; full authenticator attestation crate is deferred)

## Current state (read before executing)

Facts verified against the tree at plan time:

- **Workspace pins (`Cargo.toml` / `Cargo.lock`)**:
  - Edition `2024`, MSRV `1.96`, `#![forbid(unsafe_code)]` on all library crates, `deny.toml` license allow-list (`Apache-2.0`, `MIT`, `BSD-2-Clause`, `BSD-3-Clause`, `ISC`, etc.).
  - Already resolved in `Cargo.lock` (zero new license risk): `base64 = "=0.23.1"`, `hex = "=0.4.3"`, `hmac = "=0.13.0"`, `sha1 = "=0.11.0"`, `sha2 = "=0.11.1"`, `subtle = "=2.6.1"`, `secrecy = "=0.10.3"`, `zeroize = "=1.9.1"`, `rand = "=0.10.3"`, `rsa = "=0.10.0"`, `p256 = "=0.14.0"`, `p384 = "=0.14.0"`, `ecdsa = "=0.17.0"`, `signature = "=3.0.0"`, `url = "=2.5.9"`, `reqwest = "=0.13.5"`, `bergshamra = "=0.12.0"` (BSD-2-Clause, pure-Rust XML-DSig + Exclusive C14N, pulled in by `saml-rs = "=0.8.0"`).
  - New crate to pin: `argon2 = "=0.6.0"` (pure Rust, `MIT OR Apache-2.0`, MSRV `1.85`, default features include `alloc`, `getrandom`, `password-hash`).
- **Persistence & SQL (`crates/data/src/`)**:
  - `crates/data/src/sql.rs` is the single central registry of SQL constants loaded from `crates/data/sql/{common,postgres,mysql,sqlite}/` via `include_str!`.
  - `crates/data/src/persistence/backend/mod.rs` defines `DatabaseBackend`, `DatabaseTransaction`, `DbValue`, and dialect-aware `record_audit_log`.
  - `crates/data/src/persistence/migration/runner.rs` defines `Migrator::with_rume_core(dialect)` (`RUME_CORE_MIGRATION_ID` = `20261104_001_rume_core_schemas`, `RUME_AUDIT_MIGRATION_ID` = `20261104_002_rume_audit_schema`; both are immutable once deployed).
  - `crates/data/src/cache/backend.rs` defines `CacheBackend` and `InMemoryCacheBackend`.
- **Security & Auth (`crates/security/src/`)**:
  - `auth/schemes.rs` defines `AuthenticationScheme`, `BearerTokenScheme`, `ApiKeyScheme` (currently static `HashMap<[u8; 32], AuthUser>`), and `SessionScheme<S: SessionStore>`.
  - `auth/session.rs` defines `Session`, `SessionStore` (requiring atomic `regenerate_id`), and `InMemorySessionStore`. `auth/redis.rs` defines `RedisSessionStore` under feature `redis`.
  - `auth/policy.rs` defines synchronous `Policy<T, R>`, `RequireRole`, `RequirePermission`, `CustomPolicy`, and `authorize`.
  - `auth/layer.rs` defines `AuthLayer` and `PolicyLayer`.
  - `tenant/resolver.rs` defines `TenantResolver`, `HeaderTenantResolver`, and `SubdomainTenantResolver`.
  - `tenant/layer.rs` defines `TenantLayer`, `TenantMembershipLayer` (fail-closed by default), and `TenantQuotaLayer`.
- **Enterprise SAML (`crates/enterprise/src/saml/`)**:
  - `verify.rs` defines `SamlError`, synchronous `SamlReplayCache`, `InMemorySamlReplayCache`, `SamlSignatureVerifier`, `SamlValidationConfig`, and `SamlParser::{validate, validate_strict, validate_with_config, validate_browser_sso}`.
  - `crates/enterprise/Cargo.toml` currently has features `saml` and `enterprise = ["saml"]`, and does not yet depend on `deadpool-redis` / `redis`.
- **App & Facade (`crates/app`, `crates/core`)**:
  - `crates/core/src/lib.rs` re-exports `auth`, `security`, `tenant`, `enterprise`, `persistence`, `sql`.
  - `crates/core/Cargo.toml` wires backend features (`sqlite`, `postgres`, `mysql`, `redis`, `saml`, `enterprise`) down to sub-crates.

## Design Rulings

Autonomous engineering rulings for Milestone B (no open questions):

1. **Pure-Rust JWT verification without adding `jsonwebtoken`:** Implement `JwtVerifier` directly over workspace-pinned `rsa = "=0.10.0"`, `p256 = "=0.14.0"`, `p384 = "=0.14.0"`, `hmac = "=0.13.0"`, `sha2 = "=0.11.1"`, `base64 = "=0.23.1"`, and `subtle = "=2.6.1"`. All of these are already in `Cargo.lock`, avoid duplicate crypto trees (`multiple-versions` clean), enforce strict `alg` allow-listing (rejecting `"none"` and key-type confusion), and keep `#![forbid(unsafe_code)]`.
2. **Hermetic JWKS/OIDC HTTP boundary:** Gate `ReqwestJwksFetcher` and `ReqwestOidcDiscoveryClient` behind an opt-in `http-client` feature on `rume-security` (forwarded by `rume`). The `JwksFetcher` trait and `JwtVerifier` / `OAuth2Config` / `PkceChallenge` / `OidcDiscovery` work without `http-client` so unit tests are 100% hermetic and offline.
3. **Pure-Rust RFC 6238 TOTP & Base32:** Implement TOTP (HMAC-SHA1/SHA256/SHA512) and RFC 4648 Base32 directly in `rume-security::auth::mfa` over workspace `hmac`, `sha1`, `sha2`, `rand`, and `subtle` — zero additional TOTP crate needed, constant-time code comparison, and full `otpauth://totp/...` URI generation.
4. **Parameterized Postgres RLS & `search_path`:** `DatabaseTransaction::set_tenant_context` executes `SELECT set_config('app.current_tenant', $1, true)` (transaction-local, identical semantics to `SET LOCAL`, immune to SQL injection) after validating the tenant ID; `set_tenant_search_path` validates the schema name against `^[a-zA-Z_][a-zA-Z0-9_]{0,62}$` and executes `SELECT set_config('search_path', $1, true)`. Both fail closed with `PersistenceError::DialectMismatch` on non-Postgres dialects.
5. **`TenantPoolManager` placement:** Live in `rume-data::persistence::tenancy` (L2, alongside `DatabasePool`) and re-export from `rume-security::tenant` (L3) so L2 owns all pool lifecycle logic without violating the 7-layer DAG.
6. **Backward-compatible `ApiKeyScheme`:** Preserve `ApiKeyScheme::new().add_key(...)` for static in-memory keys while adding `ApiKeyScheme::with_store(store: impl ApiKeyStore)` for persistent store-backed lookup, expiry, revocation, and scope-to-permission mapping.
7. **Append-only auth migrations:** Never mutate `RUME_CORE_MIGRATION_ID` or `RUME_AUDIT_MIGRATION_ID`. Add `Migrator::with_rume_auth(dialect)` (`RUME_AUTH_MIGRATION_ID = "20261126_001_rume_auth_schemas"`) creating `rume_sessions`, `rume_api_keys`, `rume_roles`, `rume_role_permissions`, `rume_user_roles`, and `rume_tenants` across SQLite, Postgres, and MySQL.
8. **Async SAML replay cache alongside sync trait:** Preserve synchronous `SamlReplayCache` and `InMemorySamlReplayCache` without breaking callers; add `AsyncSamlReplayCache` (with a `SyncReplayCacheAdapter` / blanket bridge) + `RedisSamlReplayCache` (behind `rume-enterprise` feature `redis`, using atomic `SET key "1" NX EX ttl`) + `SamlParser::validate_browser_sso_async`. Never call `Handle::current().block_on(...)` inside async handlers.
9. **XML-DSig via `bergshamra = "=0.12.0"` (`rustcrypto`) + explicit runtime allow-list:** Pin `saml-rs = { version = "=0.8.0", default-features = false }` in root `Cargo.toml` so Cargo feature unification cannot re-enable `bergshamra/legacy-algorithms` (SHA-1), and build `XmlDsigSamlVerifier` on `bergshamra` (BSD-2-Clause, pure-Rust `rustcrypto` provider, already in `Cargo.lock`) using `DsigContext::new(keys_manager)` + `verify_all_document_with_source`, paired with explicit runtime algorithm allow-listing (RSA-SHA256, ECDSA-SHA256, SHA-256 digest, Enveloped Signature, Exclusive C14N), trusted-certificate pinning, and `VerifiedReference.resolved_node` `<Assertion>` binding to block XML Signature Wrapping (XSW) attacks.

## Global constraints

- **Workspace invariants**: `#![forbid(unsafe_code)]` in all library crates; Rust Edition 2024; MSRV `1.96`; concrete version pins (`=x.y.z`) in root `Cargo.toml`; all dependencies must pass `cargo deny check licenses` against `deny.toml`.
- **Strict 7-layer DAG**: L0 `rume-foundation` -> L1 `rume-observability` -> L2 `rume-data` -> L3 `rume-messaging`/`rume-security`/`rume-enterprise` -> L4 `rume-http` -> L5 `rume-app` -> L6 `rume` (`crates/core`) + `rume-macros`. Never introduce upward or cyclic dependencies.
- **Build concurrency**: Every `cargo` command MUST pass `-j 2`.
- **Zero-warning bar** (`rules/80-quality-bar.md`): `cargo fmt --all --check` and `cargo clippy -j 2 --workspace --all-targets --all-features -- -D warnings` must pass with zero warnings.
- **Execution mode (strictly sequential on `main`)**: Execute tasks `01` through `07` strictly in order on `main`, one implementer subagent at a time (`rules/30-git.md`, `rules/50-agent-conduct.md`).
- **TDD & fails-before** (`rules/50-agent-conduct.md`, `rules/80-quality-bar.md`): Task `01` scaffolds the compiling `todo!()` skeleton; tasks `02`–`06` write tests first against the skeleton (confirming fails-before), then implement minimal clean code; task `07` wires the facade and runs full end-to-end verification.
- **Commit discipline** (`rules/30-git.md`): Conventional Commits; no commits unless explicitly asked or announced up front by the controller.

## Constraints honoured

- **Constant-time secret comparisons**: All MAC, hash, token, TOTP, and API-key secret comparisons MUST use `subtle::ConstantTimeEq` (`crates/security/src/auth/schemes.rs`, `rules/85-minimalism.md`).
- **Secret redaction**: Passwords, signing keys, TOTP secrets, OAuth client secrets, and raw API keys MUST use `secrecy::SecretString` / `zeroize` and never appear in `Debug` output or `tracing` logs.
- **Existing auth & session invariants**: `crates/security/src/auth/session.rs` (`SessionStore::regenerate_id` atomicity, `in_memory_session_store_evicts_expired_sessions`), `crates/security/src/auth/schemes.rs` (cookie-tossing loop in `SessionScheme`), and `crates/security/src/auth/policy.rs` (`Policy` trait) must remain passing.
- **Existing tenant invariants**: `crates/security/src/tenant/resolver.rs` (cross-namespace collision checks in `HeaderTenantResolver`, single-label DNS boundary checks in `SubdomainTenantResolver`) and `crates/security/src/tenant/layer.rs` (`TenantMembershipLayer` fail-closed default) must remain passing.
- **Existing SAML invariants**: `crates/enterprise/src/saml/tests.rs` (all existing SAML parser, XSW, subsecond clock skew, audience restriction, and replay cache tests) must remain passing.
- **Existing migration immutability**: `crates/data/src/persistence/migration/runner.rs` (`RUME_CORE_MIGRATION_ID`, `RUME_AUDIT_MIGRATION_ID` checksums) must never change.
- **Centralized SQL assets**: All new SQL files live under `crates/data/sql/{common,postgres,mysql,sqlite}/` and are exported through `crates/data/src/sql.rs` (`rules/00-repo-layout.md`, `crates/data/src/sql.rs:3-6`).
- **Rules**: `rules/10-tech-stack.md`, `rules/40-verification.md`, `rules/50-agent-conduct.md`, `rules/70-code-rationale.md`, `rules/80-quality-bar.md`, `rules/85-minimalism.md`, `rules/testing.md`.

## Task index

- `01-deps-sql-and-skeleton.md` — Pin workspace dependencies (`=x.y.z`, verified against `deny.toml`), add SQL assets & `Migrator::with_rume_auth` in `rume-data`, and scaffold compiling module/export skeletons across `rume-data`, `rume-security`, and `rume-enterprise`.
- `02-passwords-tokens-and-mfa.md` — Implement Argon2id `PasswordHasher`, expiring HMAC-SHA256 `TokenSigner` / `SignedUrl`, RFC 6238 TOTP + hashed recovery codes, and `WebAuthnVerifier` trait in `rume-security`.
- `03-jwt-jwks-and-oidc.md` — Implement `JwtVerifier` (HS*/RS*/ES* + JWKS `kid` lookup + cached refresh + optional `ReqwestJwksFetcher` + `BearerTokenScheme` integration) and OAuth2/OIDC PKCE + state/nonce helpers in `rume-security`.
- `04-persistent-auth-stores-and-rbac.md` — Implement `ApiKeyStore` / `SqlApiKeyStore` (wired into `ApiKeyScheme`), `SqlSessionStore` (atomic `regenerate_id`), and dynamic tenant-scoped RBAC/ABAC (`RoleStore`, `SqlRoleStore`, `RequireDynamicPermission`, `AbacPolicy`, `DynamicPolicyLayer`).
- `05-multi-tenancy-resolvers-rls-and-pools.md` — Implement `PathPrefixTenantResolver`, `CachedDbTenantResolver` (with negative-caching & invalidation), Postgres RLS `DatabaseTransaction::set_tenant_context` / `set_tenant_search_path`, and `TenantPoolManager`.
- `06-saml-xmldsig-and-redis-replay.md` — Implement `XmlDsigSamlVerifier` (RSA-SHA256 + ECDSA-SHA256 + Exclusive C14N + X.509 trust pinning + XSW defense) and `AsyncSamlReplayCache` / `RedisSamlReplayCache` in `rume-enterprise`.
- `07-app-facade-wiring-and-e2e.md` — Wire new features and re-exports into `rume` (`crates/core`), add end-to-end integration tests across auth, tenancy, and SAML, and run full workspace verification.

## Done-when checklist

- [ ] All new workspace dependencies are concretely pinned (`=x.y.z`) in root `Cargo.toml` and pass `deny.toml` license checks.
- [ ] `rume-data::sql` exports all auth/tenancy SQL assets across SQLite/Postgres/MySQL, and `Migrator::with_rume_auth` applies and rolls back cleanly without altering existing core/audit migration checksums.
- [ ] `rume-security::auth::password` hashes and verifies passwords with Argon2id (OWASP defaults, `SecretString`, constant-time, async `spawn_blocking`).
- [ ] `rume-security::auth::tokens` signs and verifies expiring HMAC-SHA256 tokens and signed URLs in constant time.
- [ ] `rume-security::auth::mfa` generates/verifies RFC 6238 TOTP codes (SHA1/SHA256/SHA512), `otpauth://` URIs, hashed recovery codes, and defines `WebAuthnVerifier`.
- [ ] `rume-security::auth::jwt` verifies HS256/RS256/ES256 JWTs with JWKS `kid` lookup, cached refresh, strict `alg` allow-listing, and `BearerTokenScheme::from_jwt_verifier`.
- [ ] `rume-security::auth::oauth2` generates PKCE S256 challenges, authorization URLs, OIDC discovery parsing, and state/nonce validation via `OAuth2StateStore`.
- [ ] `rume-security::auth::{api_key, sql_session, rbac}` provide working `SqlApiKeyStore` (wired into `ApiKeyScheme`), `SqlSessionStore`, and tenant-scoped `SqlRoleStore` + `RequireDynamicPermission` / `AbacPolicy` / `DynamicPolicyLayer`, all tested against real in-memory SQLite.
- [ ] `rume-security::tenant` and `rume-data::persistence::tenancy` provide `PathPrefixTenantResolver`, `CachedDbTenantResolver`, Postgres RLS `DatabaseTransaction::set_tenant_context` / `set_tenant_search_path`, and `TenantPoolManager`.
- [ ] `rume-enterprise::saml` provides `XmlDsigSamlVerifier` (RSA-SHA256 & ECDSA-SHA256 with real self-signed X.509 test fixtures and XSW rejection tests) and `RedisSamlReplayCache` (gated under `redis` feature).
- [ ] All verification commands below pass cleanly with zero warnings.

## Verification commands

```bash
cargo fmt --all --check
cargo deny check licenses
cargo run -j 2 -p rume-cli -- sql:check crates/data/sql
cargo clippy -j 2 --workspace --all-targets --all-features -- -D warnings
cargo test -j 2 --workspace --lib --all-features
cargo test -j 2 -p rume --test integration --all-features
cargo test -j 2 --workspace --doc --all-features
```

## Flagged gaps

- **Live Postgres/Redis container tests**: Unit tests for `DatabaseTransaction::set_tenant_context`, `TenantPoolManager`, and `RedisSamlReplayCache` verify identifier validation, dialect fail-closed behavior, key formatting, TTL calculation, and command construction hermetically; live Postgres RLS and Redis `SET NX EX` integration tests live in `crates/core/tests/containers.rs` (run when Docker is available, reporting explicit `[skipping …]` if Docker is unreachable per `rules/testing.md`).
- **SAML experimental status banner**: Once Task `06` lands `XmlDsigSamlVerifier` and passes real cryptographic RSA-SHA256, ECDSA-SHA256, and XSW adversarial test fixtures, update the module doc banner in `crates/enterprise/src/saml/mod.rs` to reflect that built-in cryptographic XML-DSig verification is now available while keeping the `enterprise` Tier 3 stability note accurate.
