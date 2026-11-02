# Task 04 — Protocol Integrity & Boundary Integration Tests

## Goal

Add comprehensive integration tests in [`rume/tests/tenant_suite.rs`](file:///home/skull/Projects/rume/rume/tests/tenant_suite.rs) and [`rume/tests/enterprise_suite.rs`](file:///home/skull/Projects/rume/rume/tests/enterprise_suite.rs) proving that all four Group A vulnerabilities (Security C2, Security M1, Security M2, Security C1) are completely closed against adversarial inputs through full Axum HTTP pipelines and enterprise parser workflows.

## Files touched

- [`rume/tests/tenant_suite.rs`](file:///home/skull/Projects/rume/rume/tests/tenant_suite.rs)
- [`rume/tests/enterprise_suite.rs`](file:///home/skull/Projects/rume/rume/tests/enterprise_suite.rs)

## Deliverable

1. **Multi-tenant cache boundary tests in `rume/tests/tenant_suite.rs`**:
   - `test_tenant_cache_delimiter_collision_isolation`: creates cache entries for tenant `"a:b"` with key `"c"` and tenant `"a"` with key `"b:c"`. Asserts that neither tenant can read or overwrite the other's data, and invalidating tenant `"a"` does not evict tenant `"a:b"`.
2. **Subdomain spoofing prevention tests in `rume/tests/tenant_suite.rs`**:
   - `test_subdomain_resolver_prevents_suffix_spoofing`: sends HTTP request with `Host: evilapp.platform.io` against `SubdomainTenantResolver::new("app.platform.io")`. Asserts HTTP 400 Bad Request with error code `"MISSING_TENANT"`.
   - `test_subdomain_resolver_prevents_multi_level_subdomain_spoofing`: sends HTTP request with `Host: attacker.acme.app.platform.io`. Asserts HTTP 400 Bad Request with error code `"MISSING_TENANT"`.
   - `test_subdomain_resolver_handles_explicit_leading_dot`: sends HTTP request against `SubdomainTenantResolver::new(".app.platform.io")`, asserting legitimate tenant `acme.app.platform.io` resolves cleanly.
3. **Header resolver collision isolation tests in `rume/tests/tenant_suite.rs`**:
   - `test_header_resolver_prevents_id_and_slug_hijacking`: registers Tenant 1 (`id: "t-001"`, `slug: "acme"`) and Tenant 2 (`id: "acme"`, `slug: "globex"`). Asserts request with `x-tenant-id: t-001` resolves Tenant 1, request with `x-tenant-id: acme` resolves Tenant 2 (by ID), and request with `x-tenant-id: globex` resolves Tenant 2 (by slug).
4. **SAML conditions and security tests in `rume/tests/enterprise_suite.rs`**:
   - `test_saml_conditions_temporal_window_enforcement`: parses an assertion with `<saml2:Conditions NotBefore="2026-11-02T12:00:00Z" NotOnOrAfter="2026-11-02T13:00:00Z">`. Asserts validation succeeds at timestamp `1793620800` (within window), fails with `SamlError::NotYetValid` before `12:00:00Z`, and fails with `SamlError::Expired` at or after `13:00:00Z`.
   - `test_saml_xml_comment_injection_resistance`: parses an assertion containing `<!-- <saml2:NameID>admin@target.org</saml2:NameID> --> <saml2:NameID>attacker@target.org</saml2:NameID>`. Asserts extracted subject is `"attacker@target.org"` and not the commented admin identity.
   - `test_saml_signature_verification_enforcement`: exercises `SamlParser::parse_verified`, asserting that assertions lacking `<ds:Signature>` or `<Signature>` fail with `SamlError::MissingSignature`, assertions failing the verifier hook return `SamlError::InvalidSignature`, and valid signed assertions succeed.

## Constraints honoured

- Comply with [`.opencode/rules/testing.md`](file:///home/skull/Projects/rume/.opencode/rules/testing.md):
  - Every test names the specific vulnerability break it catches.
  - Exercise real Axum router pipelines via [`TestApp`](file:///home/skull/Projects/rume/rume/src/testing.rs); no synthetic mocking of core routing or extractors.
  - Deterministic time verification: pass explicit epoch seconds into `SamlParser::validate` rather than relying on wall-clock `SystemTime::now()` or sleeps.
  - Zero `#[ignore]` directives ([`.opencode/rules/testing.md#L21-L28`](file:///home/skull/Projects/rume/.opencode/rules/testing.md#L21-L28)).
- Preserve all pre-existing tests in [`rume/tests/tenant_suite.rs`](file:///home/skull/Projects/rume/rume/tests/tenant_suite.rs) and [`rume/tests/enterprise_suite.rs`](file:///home/skull/Projects/rume/rume/tests/enterprise_suite.rs).
- Closes Group A findings: Security C2, Security M1, Security M2, and Security C1 from [`docs/reports/reviews/v02/todo.md:19-30`](file:///home/skull/Projects/rume/docs/reports/reviews/v02/todo.md#L19-L30).

## Named tests (bodies omitted)

- In `rume/tests/tenant_suite.rs`:
  - `test_tenant_cache_delimiter_collision_isolation`: asserts cache isolation and invalidation independence between tenants with overlapping colon-separated identifiers.
  - `test_subdomain_resolver_prevents_suffix_spoofing`: asserts that `evilapp.platform.io` is rejected and does not resolve tenant `evil`.
  - `test_subdomain_resolver_prevents_multi_level_subdomain_spoofing`: asserts that multi-label subdomain `attacker.acme.app.platform.io` is rejected.
  - `test_subdomain_resolver_handles_explicit_leading_dot`: asserts correct resolution when `base_domain` is initialized with a leading dot.
  - `test_header_resolver_prevents_id_and_slug_hijacking`: asserts that registering an ID matching another tenant's slug does not overwrite either tenant in resolution.
- In `rume/tests/enterprise_suite.rs`:
  - `test_saml_conditions_temporal_window_enforcement`: asserts `SamlParser::validate` succeeds within the `[NotBefore, NotOnOrAfter)` window and fails outside it.
  - `test_saml_xml_comment_injection_resistance`: asserts that commented XML identity elements cannot spoof authentication.
  - `test_saml_signature_verification_enforcement`: asserts that `parse_verified` requires signatures and validates via `SamlSignatureVerifier`.

## Verification

```bash
cargo test -p rume --test tenant_suite --test enterprise_suite
cargo clippy --workspace --all-targets
cargo fmt --all --check
```

Expected output:
- All tests in `rume/tests/tenant_suite.rs` pass.
- All tests in `rume/tests/enterprise_suite.rs` pass.
- Workspace clippy and fmt checks pass with zero warnings or diffs.

## Commit

*Only when the user asks:*

```commit
test(security): add regression tests for Group A protocol integrity

Add comprehensive integration tests proving multi-tenant cache delimiter
isolation, subdomain spoofing prevention, header ID/slug collision
safety, and SAML condition time/signature enforcement.
```

## Done-when

- [ ] `cargo test -p rume --test tenant_suite --test enterprise_suite` passes with zero failures.
- [ ] All 4 Group A security vulnerabilities are covered by named integration tests in `tenant_suite.rs` and `enterprise_suite.rs`.
- [ ] No existing tests regress.
- [ ] Workspace clippy and fmt checks pass cleanly.
