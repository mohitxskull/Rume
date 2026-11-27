# Task 03 — JWT / JWKS Verification & OAuth2 / OIDC PKCE Helpers

## Goal

Implement built-in JWT verification (`JwtVerifier`) with JWKS key-set parsing, `kid` lookup, TTL-cached auto-refresh, and `BearerTokenScheme` integration, alongside OAuth2/OIDC authorization-code + PKCE (S256) + state/nonce validation helpers in `rume-security`.

## Files touched

- `crates/security/src/auth/jwt.rs`
- `crates/security/src/auth/oauth2.rs`
- `crates/security/src/auth/schemes.rs` (add `BearerTokenScheme::from_jwt_verifier(...)` + `JwtClaimsMapper` helper)
- `crates/security/src/auth/mod.rs` (exports)

## Deliverable

1. **JWT & JWKS Verifier (`crates/security/src/auth/jwt.rs`)**:
   - `JwtAlgorithm`: `Hs256`, `Hs384`, `Hs512`, `Rs256`, `Rs384`, `Rs512`, `Es256`, `Es384`. Explicitly rejects `"none"` and any algorithm not in `JwtValidationConfig::allowed_algorithms`.
   - `Jwk` / `JwkSet`: parses standard RFC 7517 JSON Web Key Sets (`kty`: `"RSA"` with `n`/`e`, `"EC"` with `crv` `P-256`/`P-384` + `x`/`y`, `"oct"` with `k`, plus `kid`, `use`, `key_ops`, `alg`). When parsing a `JwkSet`, **skip/ignore** entries with `use == Some(u)` where `u != "sig"` (e.g. `"enc"`), `key_ops` excluding `"verify"`, or unsupported `kty`/`crv` (real-world OIDC providers publish encryption/OKP keys alongside signing keys), while treating `use == None` as eligible (RFC 7517 §4.2). Fail closed on malformed key material for eligible keys or if the selected key is invalid.
   - `JwtValidationConfig`: `allowed_algorithms` (required non-empty; never defaults to accepting symmetric + asymmetric together), `expected_issuers`, `expected_audiences`, `leeway_secs` (default 30), `require_exp` (default `true`), `require_sub` (default `true`).
   - Header & key binding rules:
     - Reject any JWT containing a non-empty `"crit"` header parameter (RFC 7515 §4.1.11).
     - If the selected JWK specifies `"alg"`, it MUST match the JWT header `"alg"`; for EC keys, `crv` MUST match the header algorithm (`ES256` -> `P-256`, `ES384` -> `P-384`).
     - When `kid` is absent in the JWT header, do NOT trigger a JWKS refresh; fail closed unless the cached key set contains a single unambiguous key matching `kty`/`alg`.
   - `JwtClaims`: standard registered claims (`sub`, `iss`, `aud` supporting both JSON string and array-of-strings per RFC 7519 §4.1.3, `exp`, `nbf`, `iat`, `jti`) plus `extra: serde_json::Map<String, serde_json::Value>` for custom claims (`roles`, `permissions`, `tenant_id`, `scope`, etc.) and convenience `into_auth_user(...)` mapper.
   - `JwksFetcher` trait (async `BoxFuture`, returning `Result<JwkSet, AuthError>`) so tests are 100% hermetic.
   - `JwtVerifier`:
     - Static constructors (`from_hmac_secret`, `from_jwk_set`) and dynamic JWKS constructor (`from_jwks_fetcher(fetcher, config, cache_ttl, min_refresh_interval)`).
     - Automatic JWKS caching with `tokio::sync::RwLock`: on unknown `kid`, triggers a rate-limited forced refresh (bounded by `min_refresh_interval` to prevent cache-miss DoS flooding against the IdP).
     - Cryptographic verification: HMAC (`subtle::ConstantTimeEq`), RSA PKCS#1 v1.5 (`rsa = "=0.10.0"`), ECDSA P-256/P-384 (`p256 = "=0.14.0"`, `p384 = "=0.14.0"` accepting IEEE P1363 fixed-width `r || s` signatures per RFC 7515 §3.4).
   - Optional `#[cfg(feature = "http-client")] ReqwestJwksFetcher` using `reqwest = "=0.13.5"` with strict HTTPS enforcement (or explicit `.allow_http_for_tests()`), response size cap (1 MiB), and request timeout.
   - `BearerTokenScheme::from_jwt_verifier(verifier, mapper)` in `crates/security/src/auth/schemes.rs` wiring `JwtVerifier` directly into `AuthLayer`.
2. **OAuth2 / OIDC PKCE & State/Nonce Helpers (`crates/security/src/auth/oauth2.rs`)**:
   - `PkceChallenge`: generates 32-byte CSPRNG `code_verifier` (URL-safe Base64 no-pad, wrapped in `SecretString`) and `S256` `code_challenge = BASE64URL(SHA256(verifier))`, plus `verify_pkce_s256(verifier, challenge)` using `subtle::ConstantTimeEq`.
   - `OAuth2Config`: `client_id`, `client_secret: Option<SecretString>`, `authorization_endpoint`, `token_endpoint`, `redirect_uri`, `scopes: Vec<String>`, `issuer: Option<String>`, `jwks_uri: Option<String>`.
   - `OidcDiscovery`: serde model for `/.well-known/openid-configuration` (`issuer`, `authorization_endpoint`, `token_endpoint`, `jwks_uri`, `userinfo_endpoint`, `end_session_endpoint`, `response_types_supported`, `code_challenge_methods_supported`) with `OidcDiscovery::from_json` and `to_oauth2_config(...)` (verifying `issuer` matches expected issuer per OIDC Discovery §4.3). Optional `#[cfg(feature = "http-client")] ReqwestOidcDiscoveryClient`.
   - `OAuth2AuthState`: `state`, `nonce`, `pkce_verifier: SecretString`, `redirect_to: Option<String>`, `expires_at: u64`.
   - `OAuth2StateStore` trait (async `save` + atomic single-use `consume(state) -> Result<Option<OAuth2AuthState>, AuthError>`) + `InMemoryOAuth2StateStore` (evicting expired entries).
   - `OAuth2Client`: `begin_authorization(&self, store, ttl_secs, redirect_to) -> Result<(Url, OAuth2AuthState), AuthError>` building the RFC 6749 + RFC 7636 authorization URL (`response_type=code`, `client_id`, `redirect_uri`, `scope`, `state`, `nonce`, `code_challenge`, `code_challenge_method=S256`) and `validate_callback(&self, store, received_state, id_token_nonce_opt) -> Result<OAuth2AuthState, AuthError>` enforcing single-use state consumption and constant-time nonce validation.

## Constraints honoured

- `crates/security/src/auth/schemes.rs`: Preserve existing `BearerTokenScheme::new(...)` signature and behavior (`Bearer ` prefix handling, empty token rejection).
- `crates/security/src/auth/error.rs`: Expired JWTs return `AuthError::Expired`; invalid signatures, `alg: "none"`, `aud`/`iss` mismatches, and replayed OAuth2 states return `AuthError::InvalidCredentials`.
- Algorithm confusion defense: Never accept a symmetric HMAC token verified with an RSA/EC public key, and never allow `alg: "none"`.
- `#![forbid(unsafe_code)]`.

## Named tests (bodies omitted)

- `jwt_verifier_verifies_hs256_rs256_and_es256_tokens_and_rejects_tampered_signature`
- `jwt_verifier_rejects_alg_none_and_disallowed_algorithm_confusion`
- `jwt_verifier_skips_enc_and_unknown_jwks_and_rejects_crit_header`
- `jwt_verifier_validates_exp_nbf_leeway_issuer_and_single_or_array_audience`
- `jwt_verifier_refreshes_jwks_on_unknown_kid_and_rate_limits_miss_storms`
- `bearer_token_scheme_from_jwt_verifier_populates_auth_user_in_auth_layer`
- `pkce_s256_challenge_matches_rfc7636_test_vector_and_verifies_constant_time`
- `oauth2_begin_authorization_and_single_use_callback_validation_rejects_replay_and_wrong_nonce`
- `oidc_discovery_parses_document_and_rejects_issuer_mismatch`

## Code-only decisions

- JWKS cache miss refresh cooldown (`min_refresh_interval` default: `30s`) — Why: prevents unauthenticated callers sending random `kid` headers from triggering unbounded outbound HTTP requests to the IdP JWKS endpoint (DoS amplification).
- `ReqwestJwksFetcher` max response size: `1 MiB` — Why: bounds memory against compromised or misconfigured JWKS endpoints.

## Verification

```bash
cargo fmt --all --check
cargo clippy -j 2 -p rume-security --all-targets --all-features -- -D warnings
cargo test -j 2 -p rume-security --lib --all-features
```

## Commit

*(only when the user asks or controller announces commit-per-task)*

```
feat(security): add JWT/JWKS verifier and OAuth2/OIDC PKCE helpers
```

## Done-when

- [ ] `JwtVerifier` verifies HS256/HS384/HS512, RS256/RS384/RS512, and ES256/ES384 against static keys and dynamic `JwksFetcher` with rate-limited `kid`-miss refresh.
- [ ] `BearerTokenScheme::from_jwt_verifier` integrates with `AuthLayer` and extracts `AuthUser`.
- [ ] `PkceChallenge`, `OidcDiscovery`, `OAuth2StateStore` / `InMemoryOAuth2StateStore`, and `OAuth2Client` pass RFC 7636 and replay/nonce tests.
- [ ] `http-client` feature compiles cleanly with `ReqwestJwksFetcher` and `ReqwestOidcDiscoveryClient`.
- [ ] All verification commands pass with zero warnings.
