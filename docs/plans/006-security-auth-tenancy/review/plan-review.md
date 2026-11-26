# Plan 006 Review — Milestone B: Security, Auth & Multi-Tenancy

## Summary

Plan 006 is well-structured, adheres cleanly to `plan-authoring` conventions (zero inline boilerplate, compiling skeleton first, crisp 7-layer DAG placement), and makes sound core technology choices (`argon2 = "=0.6.0"` verified stable on crates.io with `MIT OR Apache-2.0` / MSRV `1.85`; `rsa = "=0.10.0"`, `p256 = "=0.14.0"`, `p384 = "=0.14.0"`, and `bergshamra = "=0.12.0"` with `#![forbid(unsafe_code)]` and `BSD-2-Clause` all verified in `Cargo.lock`; parameterized `set_config(..., true)` for Postgres RLS and `search_path`). However, **1 Critical** finding (Cargo feature unification from `saml-rs` silently re-enables SHA-1 in `bergshamra` unless blocked explicitly at runtime and via `default-features = false`) and **3 Major** findings (nullable `tenant_id` in `rume_user_roles`/`rume_roles` breaking PK/`WHERE` semantics across SQLite/Postgres/MySQL; `JwkSet` failing closed on `use != "sig"` breaking real-world OIDC providers; `XmlDsigSamlVerifier` needing explicit `verify_all` + `resolved_node` `<Assertion>` binding) should be resolved in the plan before scaffolding Task 01.

## Findings

### C1 — Cargo feature unification from `saml-rs` re-enables SHA-1 (`legacy-algorithms`) in `bergshamra`

- **Where:** `README.md` §Design Rulings #9; `01-deps-sql-and-skeleton.md` §Deliverable #1; `06-saml-xmldsig-and-redis-replay.md` §Deliverable #1 & §Code-only decisions.
- **What it says:** Task 01 pins `bergshamra = { version = "=0.12.0", default-features = false, features = ["rustcrypto"] }`. Task 06 specifies: *"Supported algorithms (allow-listed; rejects SHA-1 / MD5 / unsupported algorithms)"* and *"Reject SHA-1 XML-DSig signatures (`rsa-sha1` / `sha1` digest) by default"*.
- **Why it's a problem:** Root `Cargo.toml:77` declares `saml-rs = "=0.8.0"` with default features enabled. In `saml-rs-0.8.0/Cargo.toml:64-81`, `default = ["crypto-bergshamra"]`, which enables `bergshamra/legacy-algorithms`, `bergshamra/post-quantum`, and `bergshamra/pkcs11`. Because Cargo features are additive across the workspace, `bergshamra/legacy-algorithms` (which enables `rsa-sha1`, `ecdsa-sha1`, and `sha1` digests) remains compiled in at runtime. Furthermore, `bergshamra::DsigContext` (`bergshamra-dsig-0.12.0/src/context.rs`) has no built-in algorithm allow-list field. If `XmlDsigSamlVerifier` relies on `bergshamra`'s feature flags to block SHA-1, SHA-1 signatures and digests will silently pass verification.
- **Suggested fix:**
  1. In Task 01 (`Cargo.toml`), pin `saml-rs = { version = "=0.8.0", default-features = false }` (verified against `crates/enterprise/src/saml/mod.rs`: Rume only uses `saml-rs` namespace constants).
  2. In Task 06 (`XmlDsigSamlVerifier`), require explicit runtime allow-list inspection of `<ds:SignatureMethod Algorithm>`, `<ds:DigestMethod Algorithm>`, `<ds:CanonicalizationMethod Algorithm>`, and `<ds:Transform Algorithm>` on the matched `<ds:Signature>` node (accepting only RSA-SHA256, ECDSA-SHA256, SHA-256 digest, Enveloped Signature, and Exclusive C14N) as defense-in-depth against any downstream crate unifying `bergshamra/legacy-algorithms`. Add a named negative test `xmldsig_verifier_rejects_sha1_signature_or_digest_algorithm`.

### M1 — Nullable `tenant_id` in `rume_roles` / `rume_user_roles` breaks composite PK/`UNIQUE` and `WHERE` semantics across SQLite, Postgres, and MySQL

- **Where:** `01-deps-sql-and-skeleton.md` §Deliverable #2 (`rume_roles`, `rume_user_roles`); `04-persistent-auth-stores-and-rbac.md` §Deliverable #3 (`Role`, `RoleStore`, `SqlRoleStore`).
- **What it says:** Task 01 defines `rume_roles (id, tenant_id, name, description)` and `rume_user_roles (user_id, tenant_id, role_id)`. Task 04 defines `Role.tenant_id: Option<String>` and `assign_role(user_id, tenant_id, role_id)` / `effective_permissions(user_id, tenant_id)`.
- **Why it's a problem:**
  1. PostgreSQL and MySQL forbid `NULL` in `PRIMARY KEY (user_id, tenant_id, role_id)`.
  2. Even with a nullable `UNIQUE` index, SQL `NULL != NULL` permits duplicate global (`tenant_id IS NULL`) role names/assignments, and parameterized `WHERE user_id = $1 AND tenant_id = $2` evaluates to `UNKNOWN` when `$2` is `DbValue::Null`, silently returning zero rows for global roles.
  3. In MySQL InnoDB, `TEXT` columns cannot be part of a `PRIMARY KEY` or `UNIQUE` index without prefix lengths; all indexed/PK string columns (`id`, `prefix`, `slug`, `user_id`, `tenant_id`, `role_id`, `permission`, `name`) must be bounded `VARCHAR(...)` (matching `crates/data/sql/mysql/outbox_schema.sql`).
- **Suggested fix:** Record a Code-only decision in Task 01 and Task 04: store global (non-tenant) scope as `tenant_id TEXT NOT NULL DEFAULT ''` (SQLite/Postgres) / `VARCHAR(128) NOT NULL DEFAULT ''` (MySQL) in `rume_roles` and `rume_user_roles`, mapping `Option<String>` (`None` <-> `""`) at the `SqlRoleStore` boundary (safe because `validate_tenant_id` rejects `""`, so `""` cannot collide with any real tenant ID). Explicitly specify `VARCHAR(...)` for all PK/indexed string columns in MySQL DDL.

### M2 — `JwkSet` failing closed on `use != "sig"` rejects valid OIDC provider JWKS documents

- **Where:** `03-jwt-jwks-and-oidc.md` §Deliverable #1 (`Jwk` / `JwkSet` and `JwtVerifier`).
- **What it says:** *"`Jwk` / `JwkSet`: parses standard RFC 7517 JSON Web Key Sets (`kty`: `"RSA"` with `n`/`e`, `"EC"` with `crv` `P-256`/`P-384` + `x`/`y`, `"oct"` with `k`, plus `kid`, `use`, `alg`), failing closed on malformed keys or `use != "sig"`."*
- **Why it's a problem:** Real-world OIDC providers (Auth0, Entra ID, Okta, Keycloak, Google) frequently publish encryption keys (`"use": "enc"`) or extra key types (`"kty": "OKP"`) alongside `"use": "sig"` keys in the same `/.well-known/jwks.json`, and RFC 7517 §4.2 makes `"use"` optional. Failing the entire `JwkSet` parse when any key has `"use": "enc"`, omitted `"use"`, or an unsupported `kty` will break verification against standard IdPs. Additionally, Task 03 does not specify handling for RFC 7515 §4.1.11 `"crit"` headers, JWK `"alg"` matching, or missing `kid` behavior.
- **Suggested fix:** Update Task 03 §Deliverable #1 to specify:
  1. When parsing a `JwkSet`, **skip/ignore** entries with `use == Some(u)` where `u != "sig"` (e.g. `"enc"`), `key_ops` excluding `"verify"`, or unsupported `kty`/`crv`, while treating `use == None` as eligible (RFC 7517 §4.2). Fail closed on malformed key material for eligible keys or if the selected key is invalid.
  2. Enforce that if a selected JWK specifies `"alg"`, it MUST match the JWT header `"alg"` (and for EC keys, `crv` MUST match `ES256` -> `P-256`, `ES384` -> `P-384`).
  3. Reject any JWT containing a non-empty `"crit"` header parameter (RFC 7515 §4.1.11).
  4. When `kid` is absent in the JWT header, do not trigger a JWKS refresh; fail closed unless the key set has a single unambiguous key matching `kty`/`alg`.

### M3 — `XmlDsigSamlVerifier` needs explicit `verify_all` + `DsigContext::new` + `resolved_node` `<Assertion>` binding

- **Where:** `06-saml-xmldsig-and-redis-replay.md` §Deliverable #1 (Verification steps 1–4).
- **What it says:** *"2. Locate the exact `<ds:Signature>` whose `<ds:Reference URI="#<assertion_id>">` targets the parsed `assertion_id` (and verify the signed element is the exact `assertion_xml` / node being consumed, never an injected decoy sibling or wrapper)."*
- **Why it's a problem:** `bergshamra::verify` only verifies the **first** `<ds:Signature>` in document order (`bergshamra-dsig-0.12.0/src/verify.rs:155-167`), which in a dual-signed SAML Response is the outer `<Response>` signature rather than the `<Assertion>` signature. Furthermore, `VerifyResult::is_valid()` alone does not guarantee local digest verification for every reference (`all_reference_digests_verified()`), and matching `Reference.uri == "#{assertion_id}"` without verifying that `VerifiedReference.resolved_node` is an `<Assertion>` element in the SAML 2.0 assertion namespace with `ID == assertion_id` leaves an ambiguity for the implementer.
- **Suggested fix:** Specify in Task 06:
  1. Construct `DsigContext::new(keys_manager)` (preserving its hardened defaults: `trusted_keys_only = true`, `strict_verification = true`, `require_reference_digests = true`).
  2. Parse `raw_xml` with `uppsala::parse` and call `bergshamra::dsig::verify::verify_all_document_with_source(&ctx, &doc, Some(raw_xml))`.
  3. Require a `VerifyResult::Valid` entry where `all_reference_digests_verified()` is `true`, the `<ds:Signature>` passes the explicit algorithm allow-list (C1), and at least one `VerifiedReference` has `uri == format!("#{assertion_id}")`, `digest_verified == true`, and `resolved_node` pointing to an `<Assertion>` element in `urn:oasis:names:tc:SAML:2.0:assertion` with `ID == assertion_id`.

### m1 — Run `rume sql:check crates/data/sql` and `cargo deny check licenses` in verification

- **Where:** `README.md` §Verification commands; `01-deps-sql-and-skeleton.md` §Verification; `07-app-facade-wiring-and-e2e.md` §Verification.
- **What it says:** Neither Task 01 nor Task 07 invokes `rume sql:check`, and Task 01 treats `cargo deny check licenses` as optional.
- **Why it's a problem:** `rume sql:check` (`crates/cli/src/commands/sql_check.rs`) runs `sqlparser = "=0.64.0"` dialect-aware syntax and placeholder checks (`$1..$N` for Postgres, `?` for SQLite/MySQL) that SQLite-only unit tests cannot catch on Postgres/MySQL SQL files. `cargo-deny 0.22.0` is installed in this workspace.
- **Suggested fix:** Add `cargo run -j 2 -p rume-cli -- sql:check crates/data/sql` and `cargo deny check licenses` to Task 01's verification block, and add `sql:check` to `README.md` and Task 07.

### m2 — Externalize Postgres `set_config` SQL assets and clean up `crates/security/Cargo.toml` workspace pins

- **Where:** `01-deps-sql-and-skeleton.md` §Deliverable #2; `05-multi-tenancy-resolvers-rls-and-pools.md` §Deliverable #3.
- **What it says:** `README.md` §Constraints honoured requires all new SQL files under `crates/data/sql/` exported via `rume_data::sql`, while Task 05 executes `SELECT set_config('app.current_tenant', $1, true)` and `SELECT set_config('search_path', $1, true)` without listing SQL assets for them in Task 01. Also, `crates/security/Cargo.toml` currently pins `sha2`/`hmac` inline and does not yet list `subtle`, `secrecy`, or `url`.
- **Suggested fix:** Add `crates/data/sql/postgres/pg_set_tenant_context.sql` and `pg_set_tenant_search_path.sql` (exported in `crates/data/src/sql.rs`) to Task 01, and note migrating `crates/security/Cargo.toml`'s inline `sha2`/`hmac` to `.workspace = true` alongside adding `subtle`, `secrecy`, and `url`.

### m3 — Remove `rume-app` from Task 07 summary/title

- **Where:** `README.md` §Task index (`07-app-facade-wiring-and-e2e.md`); `07-app-facade-wiring-and-e2e.md` title.
- **What it says:** References wiring exports into `rume-app` and `rume` (`crates/core`), whereas `07-app-facade-wiring-and-e2e.md` §Files touched only lists `crates/core/`.
- **Why it's a problem:** `crates/app/Cargo.toml` intentionally does not depend on `rume-security`, `rume-data`, or `rume-enterprise`; `crates/core` is the re-export facade. Mentioning `rume-app` could mislead an implementer into adding unnecessary crate dependencies.
- **Suggested fix:** Update `README.md` §Task index and Task 07's title to reference `rume` (`crates/core`) only.

## Positive notes

- **Dependency hygiene & zero license risk:** Verified directly against `Cargo.lock` and crates.io — `argon2 = "=0.6.0"` is a stable release (`MIT OR Apache-2.0`, MSRV `1.85`), `rsa = "=0.10.0"` has the `pkcs1v15` feature, `p256`/`p384 = "=0.14.0"` include `ecdsa` by default, and `bergshamra = "=0.12.0"` is pure Rust (`#![forbid(unsafe_code)]`, `BSD-2-Clause`) already in `Cargo.lock`. Implementing JWT/JWKS and TOTP directly on existing workspace crypto crates avoids duplicate crypto dependency trees.
- **Parameterized Postgres RLS & `search_path`:** Using `SELECT set_config('app.current_tenant', $1, true)` and `SELECT set_config('search_path', $1, true)` with `validate_tenant_id` / `validate_sql_identifier` (`^[a-zA-Z_][a-zA-Z0-9_]{0,62}$`) and failing closed with `PersistenceError::DialectMismatch` on non-Postgres dialects eliminates SQL injection, prevents `public` schema smuggling, and guarantees transaction-local reset (verified `PostgresTransaction::dialect()` returns `"postgres"` and `query_scalar_params` decodes `TEXT` cleanly).
- **Clean 7-layer DAG & append-only migrations:** Placing `TenantPoolManager` in `rume-data` (L2) and re-exporting from `rume-security` (L3) preserves DAG acyclicity; keeping `RUME_CORE_MIGRATION_ID` and `RUME_AUDIT_MIGRATION_ID` untouched and introducing `Migrator::with_rume_auth` avoids checksum regressions.
- **DoS & replay hardening:** Rate-limiting JWKS `kid`-miss refreshes (`min_refresh_interval = 30s`), capping JWKS HTTP responses at `1 MiB`, negative-caching unknown tenants in `CachedDbTenantResolver`, returning the matched TOTP time step for replay prevention, and using atomic `SET NX EX` in `RedisSamlReplayCache` without `block_on` are all strong production safeguards that should be preserved.

## Verdict

**Changes Requested** — Resolve **C1** and **M1–M3** (plus polish **m1–m3**) in the plan files before scaffolding Task 01. All fixes are localized plan edits and require no architectural rework.
