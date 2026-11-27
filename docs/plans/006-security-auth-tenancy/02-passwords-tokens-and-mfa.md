# Task 02 — Password Hashing (Argon2id), Signed Tokens & MFA (TOTP + WebAuthn Hook)

## Goal

Implement production-grade password hashing (`PasswordHasher` using Argon2id with OWASP defaults), expiring HMAC-SHA256 signed tokens and signed URLs (`TokenSigner`, `SignedUrl`), RFC 6238 TOTP MFA with hashed recovery codes (`TotpConfig`, `TotpSecret`, `RecoveryCodes`), and the pluggable `WebAuthnVerifier` trait in `rume-security`.

## Files touched

- `crates/security/src/auth/password.rs`
- `crates/security/src/auth/tokens.rs`
- `crates/security/src/auth/mfa.rs`
- `crates/security/src/auth/mod.rs` (ensure clean re-exports)

## Deliverable

1. **Argon2id Password Hashing (`crates/security/src/auth/password.rs`)**:
   - `PasswordHasherConfig`: OWASP 2024 Argon2id defaults (`m_cost = 19_456` KiB, `t_cost = 2`, `p_cost = 1`, `output_len = 32`), plus a `.low_cost_for_tests()` constructor (explicitly doc-warned for test speed only) and optional pepper (`Option<SecretString>`).
   - `PasswordHasher`:
     - `hash_password(&self, password: &SecretString) -> Result<String, AuthError>` returning standard PHC string (`$argon2id$v=19$...`) with a fresh CSPRNG 16-byte salt.
     - `verify_password(&self, password: &SecretString, phc_hash: &str) -> Result<bool, AuthError>` using constant-time verification and rejecting non-Argon2id algorithms by default.
     - `needs_rehash(&self, phc_hash: &str) -> bool` returning `true` when stored parameters differ from the configured policy (supports transparent parameter upgrades on login).
     - `hash_password_async` and `verify_password_async` running CPU-bound hashing on `tokio::task::spawn_blocking` so Tokio async workers are never stalled.
2. **Signed Tokens & Signed URLs (`crates/security/src/auth/tokens.rs`)**:
   - `TokenSigner`: holds a `SecretString` (or `Zeroizing<Vec<u8>>`) signing key (minimum 32 bytes enforced on construction) and supports optional previous keys for zero-downtime key rotation.
   - `SignedTokenClaims`: purpose (`purpose: String`, e.g. `"email_verify"`, `"password_reset"`, `"download"`), `subject: String`, `expires_at: u64`, `issued_at: u64`, `nonce: String` (unique token ID for optional single-use replay tracking), and `metadata: BTreeMap<String, String>`.
   - Compact URL-safe Base64 (no-pad) format `<payload_b64>.<hmac_sha256_b64>`, verified with `subtle::ConstantTimeEq`.
   - Purpose binding: `verify(&self, token: &str, expected_purpose: &str, now_epoch_sec: u64)` rejects wrong purpose, tampered payload/signature, or expired tokens (`now_epoch_sec >= expires_at`).
   - `SignedUrl`: signs any `url::Url` by appending `expires` and `sig` query parameters (computing HMAC-SHA256 over canonical method + path + sorted query parameters excluding `sig`), and verifies expiry + signature in constant time without mutating unrelated query parameters.
3. **TOTP MFA & WebAuthn Hook (`crates/security/src/auth/mfa.rs`)**:
   - `TotpAlgorithm` (`Sha1` default per RFC 6238 / authenticator compatibility, `Sha256`, `Sha512`).
   - `TotpConfig` (`algorithm`, `digits: u32` default 6, allowed 6..=8, `period_secs: u64` default 30, `skew_steps: u64` default 1).
   - `TotpSecret`: CSPRNG-generated raw bytes wrapped in `Zeroizing<Vec<u8>>`, RFC 4648 Base32 encode/decode (no-pad, case-insensitive decode), `otpauth_uri(issuer, account)` producing percent-encoded `otpauth://totp/<issuer>:<account>?secret=...&issuer=...&algorithm=...&digits=...&period=...`.
   - `generate_code(timestamp_secs)` and `verify_code(code, timestamp_secs)` returning `Option<u64>` (the matched time step, so callers can store `last_used_step` to block TOTP replay within the same 30s window) with `subtle::ConstantTimeEq`.
   - `RecoveryCodeSet`: generates `N` high-entropy human-readable recovery codes (e.g. `XXXX-XXXX-XXXX`), returns plaintext codes once to the user, and stores only salted SHA-256 hashes for constant-time verification and single-use consumption.
   - `WebAuthnVerifier` trait + `WebAuthnChallenge` / `WebAuthnAssertion` / `WebAuthnVerification` types: clean async boundary (`BoxFuture`) allowing downstream crates or apps to plug in `webauthn-rs` without forcing heavy attestation dependencies into `rume-security`.

## Constraints honoured

- `rules/70-code-rationale.md`: Document load-bearing constants (`19_456` KiB / `t=2` / `p=1` OWASP Argon2id defaults; `32`-byte minimum HMAC key length; `skew_steps = 1`).
- `rules/85-minimalism.md`: Never simplify away constant-time comparison (`subtle::ConstantTimeEq`) or `SecretString`/`Zeroizing` redaction.
- `crates/security/src/auth/error.rs`: Map expired tokens to `AuthError::Expired` and tampered/invalid credentials to `AuthError::InvalidCredentials`.
- `#![forbid(unsafe_code)]`.

## Named tests (bodies omitted)

- `argon2id_hash_and_verify_roundtrip_and_rejects_wrong_password`
- `argon2id_uses_unique_salts_and_detects_needs_rehash_on_param_upgrade`
- `argon2id_async_helpers_execute_on_blocking_pool_and_preserve_pepper`
- `token_signer_rejects_short_key_tampered_payload_wrong_purpose_and_expired_token`
- `token_signer_supports_key_rotation_with_previous_keys`
- `signed_url_roundtrip_rejects_tampered_query_params_and_expired_url`
- `totp_matches_rfc6238_test_vectors_for_sha1_sha256_sha512`
- `totp_verify_accepts_skew_window_and_returns_matched_step_for_replay_prevention`
- `totp_otpauth_uri_escapes_issuer_and_account_and_roundtrips_base32`
- `recovery_codes_verify_once_and_never_store_plaintext`

## Code-only decisions

- OWASP Argon2id default parameters: `m_cost = 19_456` (19 MiB), `t_cost = 2`, `p_cost = 1` — Why: OWASP Password Storage Cheat Sheet minimum recommended Argon2id configuration (19 MiB, 2 iterations, 1 thread).
- Minimum `TokenSigner` key length: `32` bytes (256 bits) — Why: matches HMAC-SHA256 output size to prevent weak-key brute force.

## Verification

```bash
cargo fmt --all --check
cargo clippy -j 2 -p rume-security --all-targets --all-features -- -D warnings
cargo test -j 2 -p rume-security --lib --all-features
```

## Commit

*(only when the user asks or controller announces commit-per-task)*

```
feat(security): add Argon2id password hashing, signed tokens/URLs, and TOTP MFA
```

## Done-when

- [ ] `PasswordHasher` hashes/verifies Argon2id PHC strings synchronously and asynchronously (`spawn_blocking`), supports pepper and `needs_rehash`, and never leaks secrets in `Debug`.
- [ ] `TokenSigner` and `SignedUrl` enforce minimum 32-byte key, constant-time HMAC-SHA256 verification, purpose binding, expiry, and key rotation.
- [ ] `TotpSecret` passes RFC 6238 SHA-1, SHA-256, and SHA-512 test vectors, generates valid `otpauth://` URIs, and `RecoveryCodeSet` verifies hashed single-use codes.
- [ ] `WebAuthnVerifier` trait and types compile cleanly and are exported from `rume_security::auth`.
- [ ] All verification commands pass with zero warnings.
