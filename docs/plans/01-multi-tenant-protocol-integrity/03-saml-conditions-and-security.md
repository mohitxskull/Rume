# Task 03 — SAML Conditions Parsing & Signature Verification

## Goal

Parse temporal validity conditions (`NotBefore` and `NotOnOrAfter`) from SAML 2.0 assertions into epoch timestamps, strip XML comments to prevent identity spoofing via comment injection, and add an extensible signature verification hook in [`rume/src/enterprise/saml.rs`](file:///home/skull/Projects/rume/rume/src/enterprise/saml.rs).

## Files touched

- [`rume/src/enterprise/saml.rs`](file:///home/skull/Projects/rume/rume/src/enterprise/saml.rs)

## Deliverable

1. **XML Comment Stripping**:
   - Strip XML comments (`<!-- ... -->`) from input XML before tag and attribute extraction, preventing attackers from injecting identity tags (such as `<saml2:NameID>`) inside comments.
2. **Conditions Timestamp Parsing in `SamlParser::parse`**:
   - Locate `<saml2:Conditions>`, `<saml:Conditions>`, or `<Conditions>` element in the sanitized XML.
   - Extract `NotBefore` and `NotOnOrAfter` attribute values.
   - Parse ISO 8601 / RFC 3339 UTC timestamps (e.g. `2026-11-02T12:00:00Z` or offset `+00:00`) as well as numeric epoch seconds strings into `u64` epoch seconds.
   - Populate `assertion.not_before` and `assertion.not_on_or_after` with `Some(epoch_seconds)` (or `None` when omitted). Return `SamlError::Malformed` on unparseable datetime formats.
3. **Signature Verification Hook**:
   - Add error variants to [`SamlError`](file:///home/skull/Projects/rume/rume/src/enterprise/saml.rs#L7-L24):
     - `#[error("missing XML signature in SAML assertion")] MissingSignature`
     - `#[error("invalid SAML signature: {0}")] InvalidSignature(String)`
   - Define a pluggable verifier trait:
     ```rust
     pub trait SamlSignatureVerifier: Send + Sync + std::fmt::Debug {
         fn verify_signature(&self, raw_xml: &str) -> Result<(), SamlError>;
     }
     ```
   - Provide `SamlParser::parse_verified(xml: &str, verifier: &dyn SamlSignatureVerifier) -> Result<SamlAssertion, SamlError>` that requires the presence of an XML signature element (`<ds:Signature>` or `<Signature>`) and validates it via the verifier hook before returning the assertion.
   - Retain `SamlParser::parse(xml: &str)` for unverified parsing or testing.
4. Add unit tests in `#[cfg(test)] mod tests` in [`rume/src/enterprise/saml.rs`](file:///home/skull/Projects/rume/rume/src/enterprise/saml.rs).

## Constraints honoured

- Preserve existing [`SamlAssertion`](file:///home/skull/Projects/rume/rume/src/enterprise/saml.rs#L27-L43) struct definition, fields, and types (`not_before: Option<u64>`, `not_on_or_after: Option<u64>`).
- Preserve existing [`SamlParser::validate`](file:///home/skull/Projects/rume/rume/src/enterprise/saml.rs#L159-L201) signature and logic: `NotYetValid` and `Expired` errors are retained and now actively triggered when assertions contain `<Conditions>`.
- Preserve existing test in [`rume/tests/enterprise_suite.rs:32-82`](file:///home/skull/Projects/rume/rume/tests/enterprise_suite.rs#L32-L82) (`test_saml_assertion_parsing_and_validation`).
- Comply with [`.opencode/rules/70-code-rationale.md`](file:///home/skull/Projects/rume/.opencode/rules/70-code-rationale.md#L23-L36) with `Why:` comments at comment stripping and signature hook decision sites.
- Comply with [`.opencode/rules/85-minimalism.md`](file:///home/skull/Projects/rume/.opencode/rules/85-minimalism.md#L8-L25): implement lightweight, robust ISO-8601 parsing without adding heavy external dependencies.
- Closes Security C1 finding from [`docs/reports/reviews/v02/todo.md:28-29`](file:///home/skull/Projects/rume/docs/reports/reviews/v02/todo.md#L28-L29) and [`docs/reports/reviews/v02/scratch/group-A-findings.md:108-138`](file:///home/skull/Projects/rume/docs/reports/reviews/v02/scratch/group-A-findings.md#L108-L138).

## Named tests (bodies omitted)

- `saml_parser_extracts_iso8601_conditions_timestamps`: asserts that `<saml2:Conditions NotBefore="2026-11-02T12:00:00Z" NotOnOrAfter="2026-11-02T13:00:00Z">` populates `assertion.not_before` and `assertion.not_on_or_after` with corresponding epoch seconds.
- `saml_validate_rejects_expired_assertion`: validates an assertion whose `not_on_or_after` is earlier than `current_epoch_sec`, asserting `Err(SamlError::Expired { .. })`.
- `saml_validate_rejects_not_yet_valid_assertion`: validates an assertion whose `not_before` is later than `current_epoch_sec`, asserting `Err(SamlError::NotYetValid { .. })`.
- `saml_parser_strips_xml_comments_preventing_identity_injection`: XML containing `<!-- <saml2:NameID>admin@corp.com</saml2:NameID> --> <saml2:NameID>user@corp.com</saml2:NameID>` parses `subject` as `"user@corp.com"`.
- `saml_parse_verified_requires_signature_element`: calling `parse_verified` on XML lacking a signature element returns `Err(SamlError::MissingSignature)`.
- `saml_parse_verified_delegates_to_verifier_hook`: calling `parse_verified` invokes the provided `SamlSignatureVerifier`; asserts failure if the verifier returns an error, and success if the verifier approves.

## Code-only decisions

- XML comment stripping:
  ```rust
  // Why: Stripping XML comments prior to tag matching prevents XML comment injection attacks
  // where malicious actors insert privileged identity elements (e.g. admin NameID) inside comments,
  // tricking substring-based tag matchers into authenticating the wrong principal.
  fn strip_xml_comments(xml: &str) -> String { ... }
  ```

- Extensible signature verification hook:
  ```rust
  // Why: XML-DSig signature verification requires IdP-specific X.509 certificate validation
  // or public key cryptography. Providing a pluggable verifier trait allows callers and enterprise
  // integrations to enforce strict cryptographic signature checks without binding the core crate
  // to a specific heavy crypto backend.
  pub trait SamlSignatureVerifier: Send + Sync + std::fmt::Debug {
      fn verify_signature(&self, raw_xml: &str) -> Result<(), SamlError>;
  }
  ```

## Verification

```bash
cargo test -p rume enterprise::saml
cargo clippy -p rume --all-targets
```

Expected output:
- `test enterprise::saml::tests::saml_parser_extracts_iso8601_conditions_timestamps ... ok`
- `test enterprise::saml::tests::saml_validate_rejects_expired_assertion ... ok`
- `test enterprise::saml::tests::saml_validate_rejects_not_yet_valid_assertion ... ok`
- `test enterprise::saml::tests::saml_parser_strips_xml_comments_preventing_identity_injection ... ok`
- `test enterprise::saml::tests::saml_parse_verified_requires_signature_element ... ok`
- `test enterprise::saml::tests::saml_parse_verified_delegates_to_verifier_hook ... ok`
- Clippy exits 0 with zero warnings.

## Commit

*Only when the user asks:*

```commit
feat(enterprise): parse SAML conditions and add signature verification hook

Parse NotBefore and NotOnOrAfter timestamps from SAML Conditions, strip
XML comments to prevent injection, and add a pluggable signature
verification hook. Closes Security C1.
```

## Done-when

- [ ] `SamlParser::parse` extracts `NotBefore` and `NotOnOrAfter` from `<Conditions>` into `assertion.not_before` and `assertion.not_on_or_after`.
- [ ] `SamlParser::validate` rejects assertions with expired timestamps or not-yet-valid timestamps.
- [ ] Identity tags placed inside XML comments are ignored during parsing.
- [ ] `SamlParser::parse_verified` enforces signature presence and validates via `SamlSignatureVerifier`.
- [ ] Existing enterprise tests pass without regressions.
