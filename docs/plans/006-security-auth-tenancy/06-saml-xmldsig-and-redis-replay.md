# Task 06 — Production SAML 2.0 XML-DSig Verifier & Redis Replay Cache

## Goal

Implement a real cryptographic XML-DSig verifier (`XmlDsigSamlVerifier` supporting enveloped RSA-SHA256 and ECDSA-SHA256 signatures, Exclusive C14N canonicalization, X.509 certificate trust pinning, and anti-XSW assertion-ID binding) and a distributed `RedisSamlReplayCache` (via `AsyncSamlReplayCache` + `SamlParser::validate_browser_sso_async`) in `rume-enterprise`.

## Files touched

- `crates/enterprise/src/saml/xmldsig.rs`
- `crates/enterprise/src/saml/redis_replay.rs`
- `crates/enterprise/src/saml/verify.rs` (add `AsyncSamlReplayCache`, `SyncReplayCacheAdapter`, `SamlParser::validate_with_async_replay`, `SamlParser::validate_browser_sso_async`)
- `crates/enterprise/src/saml/mod.rs` (exports + update doc banner)
- `crates/enterprise/src/enterprise/mod.rs` (exports)
- `crates/enterprise/src/saml/tests.rs` (add real RSA-SHA256, ECDSA-SHA256, XSW, and replay cache tests)

## Deliverable

1. **Cryptographic XML-DSig Verifier (`crates/enterprise/src/saml/xmldsig.rs`)**:
   - `XmlDsigSamlVerifier` implementing `SamlSignatureVerifier`.
   - Constructed with one or more trusted IdP X.509 certificates (`from_pem_cert`, `from_der_cert`, `from_pem_certs` supporting IdP certificate rollover). Rejects empty certificate sets or malformed PEM/DER certificates on construction.
   - Supported algorithms (allow-listed; rejects SHA-1 / MD5 / unsupported algorithms):
     - Signature methods: RSA-SHA256 (`http://www.w3.org/2001/04/xmldsig-more#rsa-sha256`), ECDSA-SHA256 (`http://www.w3.org/2001/04/xmldsig-more#ecdsa-sha256`).
     - Digest method: SHA-256 (`http://www.w3.org/2001/04/xmlenc#sha256`).
     - Canonicalization / Transforms: Enveloped Signature (`http://www.w3.org/2000/09/xmldsig#enveloped-signature`) + Exclusive C14N (`http://www.w3.org/2001/10/xml-exc-c14n#`, with or without comments).
   - Verification steps (using `bergshamra` `rustcrypto` provider):
     1. Reject duplicate `ID` attributes across the document (XSW defense).
     2. Construct `DsigContext::new(keys_manager)` loaded strictly with the caller-pinned trusted IdP X.509 certificate(s) (preserving hardened defaults: `trusted_keys_only = true`, `strict_verification = true`, `require_reference_digests = true`) — NEVER trust an attacker-embedded `<ds:KeyInfo><ds:X509Certificate>` unless it matches a pinned trusted certificate.
     3. Parse `raw_xml` with `uppsala::parse` and invoke `bergshamra::dsig::verify::verify_all_document_with_source(&ctx, &doc, Some(raw_xml))` (so dual-signed `<Response>` + `<Assertion>` documents verify all signatures, not just the first in document order).
     4. Find a `VerifyResult::Valid` entry where:
        - `all_reference_digests_verified()` is `true`;
        - Explicit runtime algorithm allow-list inspection of `<ds:SignatureMethod Algorithm>`, `<ds:DigestMethod Algorithm>`, `<ds:CanonicalizationMethod Algorithm>`, and `<ds:Transform Algorithm>` on the matched `<ds:Signature>` node passes (defense-in-depth against any downstream crate unifying `bergshamra/legacy-algorithms` / SHA-1);
        - At least one `VerifiedReference` has `uri == format!("#{assertion_id}")`, `digest_verified == true`, and `resolved_node` pointing to an `<Assertion>` element in `urn:oasis:names:tc:SAML:2.0:assertion` with `ID == assertion_id`.
     5. Return `Ok(())` only if all checks above succeed; otherwise return `Err(SamlError::InvalidSignature(_))` or `Err(SamlError::SignatureWrappingDetected(_))`.
2. **Async & Redis SAML Replay Cache (`crates/enterprise/src/saml/redis_replay.rs` + `verify.rs`)**:
   - `AsyncSamlReplayCache` trait (`max_clock_skew_secs(&self) -> u64`, `record_if_absent_async<'a>(&'a self, assertion_id: &'a str, expires_at: u64, now_epoch_sec: u64) -> Pin<Box<dyn Future<Output = Result<bool, SamlError>> + Send + 'a>>`).
   - `SyncReplayCacheAdapter<T: SamlReplayCache>` implementing `AsyncSamlReplayCache` so `InMemorySamlReplayCache` works with async validation seamlessly.
   - `SamlParser::validate_with_async_replay` and `SamlParser::validate_browser_sso_async` running all synchronous assertion validations first (signature verified, `AuthnStatement`, time conditions, audience, issuer, destination, recipient, `InResponseTo`, clock skew vs `cache.max_clock_skew_secs()`) and then awaiting `cache.record_if_absent_async(...)`.
   - `#[cfg(feature = "redis")] RedisSamlReplayCache`:
     - Wraps `deadpool_redis::Pool`, configurable key prefix (default `"rume:saml:replay:"`), and `max_clock_skew_secs` (default `300`).
     - Enforces `now_epoch_sec < expires_at` before touching Redis (returning `Err(SamlError::Expired { .. })`).
     - Computes `ttl_secs = (expires_at - now_epoch_sec).max(1)` and executes atomic `SET <prefix><assertion_id> "1" NX EX <ttl_secs>`.
     - Returns `Ok(true)` when Redis returns `OK` (fresh), `Ok(false)` when Redis returns `nil` (replayed), or `Err(SamlError::Malformed(..))` on Redis infrastructure error (fail-closed, never silently accepting on Redis outage).

## Constraints honoured

- `crates/enterprise/src/saml/verify.rs`: Preserve synchronous `SamlReplayCache`, `InMemorySamlReplayCache`, `SamlSignatureVerifier`, and all existing `SamlParser` methods untouched.
- `crates/enterprise/src/saml/tests.rs`: All existing SAML tests must continue to pass.
- Never trust embedded `<X509Certificate>` in untrusted SAML XML without matching a pinned IdP certificate.
- Never call `block_on` inside async code.
- `#![forbid(unsafe_code)]`.

## Named tests (bodies omitted)

- `xmldsig_verifier_accepts_valid_rsa_sha256_enveloped_assertion` (generates real RSA key + self-signed X.509 cert in test, signs SAML assertion with Exclusive C14N + RSA-SHA256, verifies via `SamlParser::parse_signed`)
- `xmldsig_verifier_accepts_valid_ecdsa_sha256_enveloped_assertion` (generates real P-256 ECDSA key + self-signed X.509 cert in test, signs SAML assertion, verifies via `SamlParser::parse_signed`)
- `xmldsig_verifier_rejects_tampered_subject_or_attributes`
- `xmldsig_verifier_rejects_untrusted_embedded_certificate`
- `xmldsig_verifier_rejects_sha1_signature_or_digest_algorithm`
- `xmldsig_verifier_rejects_xsw_signed_decoy_assertion_wrapping_attack`
- `validate_browser_sso_async_enforces_replay_and_clock_skew_via_async_cache`
- `redis_saml_replay_cache_key_formatting_and_expired_assertion_rejection`

## Code-only decisions

- Default Redis replay key prefix: `"rume:saml:replay:"`, default `max_clock_skew_secs = 300` — Why: matches `InMemorySamlReplayCache::default()` and `RedisSessionStore` prefix naming convention (`"rume:<subsystem>:"`).
- Reject SHA-1 XML-DSig signatures (`rsa-sha1` / `sha1` digest) by default — Why: SHA-1 collision attacks break XML-DSig integrity; modern SAML 2.0 IdPs use RSA-SHA256 or ECDSA-SHA256.

## Verification

```bash
cargo fmt --all --check
cargo clippy -j 2 -p rume-enterprise --all-targets --all-features -- -D warnings
cargo test -j 2 -p rume-enterprise --lib --all-features
```

## Commit

*(only when the user asks or controller announces commit-per-task)*

```
feat(enterprise): add XML-DSig SAML verifier and Redis SAML replay cache
```

## Done-when

- [ ] `XmlDsigSamlVerifier` verifies real RSA-SHA256 and ECDSA-SHA256 enveloped SAML assertions using Exclusive C14N against pinned X.509 certificates.
- [ ] `XmlDsigSamlVerifier` rejects tampered assertions, untrusted embedded certs, and XSW wrapping attacks.
- [ ] `AsyncSamlReplayCache`, `SamlParser::validate_browser_sso_async`, and `#[cfg(feature = "redis")] RedisSamlReplayCache` compile and pass all tests.
- [ ] `crates/enterprise/src/saml/mod.rs` doc banner accurately reflects production XML-DSig support.
- [ ] All verification commands pass with zero warnings.
