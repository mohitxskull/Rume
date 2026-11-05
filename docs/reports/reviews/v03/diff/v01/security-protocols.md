# Security Protocols Diff Review — v03 (v01)

## Summary

This diff review evaluates the security protocols, enterprise integrations, and authentication subsystems in Rume across commits `cf82559..HEAD`, auditing `crates/core/src/security/` (`audit.rs`, `csrf.rs`, `headers.rs`), `crates/core/src/enterprise/` (`saml.rs`, `scim.rs`), `crates/core/src/auth/` (`schemes.rs`, `session.rs`), `crates/core/src/env/` (`secrets.rs`), and associated integration test suites. Substantial security hardening has landed: SQL injection in `SqlAuditLogger` has been eliminated with correlation IDs preserved (v03 C3), unverified SAML assertion consumption in production is deprecated in favor of mandatory signature verification (v03 C2), SAML claim extraction is now strictly anchored to the assertion subtree (v03 C1), multi-cookie inspection defeats cookie-tossing denial of service in CSRF validation (v03 M2), metadata secrets are redacted during audit serialization (v03 M3), SCIM entities support ETag versioning (v03 M4), and `SessionScheme` no longer degrades authenticated sessions to guests on provider faults (v03 M6). However, the diff **fails** review and cannot be accepted as-is: the new signature reference binding in `SamlParser` erroneously rejects standard signed SAML Response envelopes (finding M1), and `CsrfConfig` continues to default to unauthenticated, insecure tokens vulnerable to subdomain tossing (finding M2).

---

## Prior decisions

- **v03 Security C1 (SAML 2.0 Parser Vulnerable to XML Signature Wrapping (XSW) Attacks via Unbound Global Tag Extraction and Namespace Prefix Evasion)** — **CHALLENGED (Defective Remediation)**.
  Dynamic namespace prefix scanning (`:Assertion` with alphanumeric prefix validation) and subtree isolation were added in `crates/core/src/enterprise/saml.rs:214–261`. All identity claims (`Issuer`, `Subject`, `Audience`, `Conditions`, `Attributes`) are now extracted strictly from `assertion_xml`, successfully preventing Type-1 Envelope Injection attacks. However, the newly added cryptographic Reference URI validation in lines 276–287 is defective: it assumes that only the `<Assertion>` element can be signed, causing valid enterprise SAML responses where the `<Response>` envelope is signed (standard in Okta, Azure AD, and ADFS) to be erroneously rejected with `SignatureWrappingDetected`, while remaining fragile against quote and namespace variants. Reported as finding M1.

- **v03 Security C2 (Public `SamlParser::parse()` Bypasses Cryptographic Verification in Production)** — **RESOLVED**.
  `SamlParser::parse(xml)` in `crates/core/src/enterprise/saml.rs:183–186` is now annotated with `#[deprecated(note = "use SamlParser::parse_signed() in production or SamlParser::parse_unverified() for tests")]`. Production code is directed to `SamlParser::parse_signed(xml, verifier)`, which mandates a reference to `&dyn SamlSignatureVerifier`. Offline testing and diagnostic parsing are segregated into `SamlParser::parse_unverified(xml)`.

- **v03 Security C3 (SQL Injection Vulnerability in `SqlAuditLogger::record()` via String Formatting and Incomplete MySQL Escape Handling)** — **RESOLVED**.
  `SqlAuditLogger::record()` in `crates/core/src/security/audit.rs:158–215` no longer relies on naive single-quote replacement. All text fields and JSON payloads are processed through `escape_sql_literal()`, which escapes `'`, `\`, `\0`, `\n`, `\r`, and `\x1a` (`\Z`), neutralizing backslash evasion attacks under MySQL's default escape mode. Furthermore, `record.id` is preserved during insertion (`let id = escape_sql_literal(&record.id);`), fixing the correlation ID overwrite. Verified by integration test `test_audit_logging_sql_injection_resilience_and_id_preservation` in `crates/core/tests/integration/security.rs:280–302`. (Note: Cross-dialect escaping trade-offs in PostgreSQL/SQLite are noted in finding m2).

- **v03 Security M1 (Insecure Defaults in `CsrfConfig` Permit Subdomain Cookie-Tossing and Lack Mandatory HMAC Signing)** — **REAFFIRMED (Unresolved)**.
  `CsrfConfig::default()` in `crates/core/src/security/csrf.rs:64–77` remains unchanged with `secret: None` and `cookie_secure: false`. When instantiated via `CsrfLayer::new()`, `verify_token` accepts any non-empty string, and CSRF cookies are transmitted without `Secure` transport flags. Double-submit validation remains vulnerable to subdomain cookie-tossing out of the box unless developers manually supply an HMAC key. Re-reported as finding M2.

- **v03 Security M2 (Subdomain Cookie-Tossing Causes Denial of Service via First-Cookie Selection Bias in `CsrfService`)** — **RESOLVED**.
  In `crates/core/src/security/csrf.rs:324–379`, `CsrfService::call` replaces `find_map` with a full-header filter collecting all matching cookie values into `Vec<&str>`. It verifies whether *any* matching cookie possesses a valid signature (`any_valid`) and whether *any* valid cookie matches the incoming header token (`has_valid_match`) using `safe_equal`. An attacker tossing an invalid cookie on a parent domain can no longer mask a valid host-scoped signed cookie. Verified in `test_csrf_signed_tokens_and_cookie_tossing_protection` in `crates/core/tests/integration/security.rs:145–205`.

- **v03 Security M3 (Lack of Secret Redaction in Audit Logging Metadata)** — **RESOLVED**.
  In `crates/core/src/security/audit.rs:184–190`, before serializing metadata into JSON, `record.metadata` is iterated through `crate::env::secrets::redact_value(k, v)`. Any key matching secret heuristics (`APIKEY`, `PASSWORD`, `TOKEN`, `AUTHORIZATION`, `SESSION`, etc.) or values matching credential URLs have their values scrubbed to `[redacted]` before persistent storage in `rume_audit_log`.

- **v03 Security M4 (SCIM 2.0 Entities Lack `meta.version` (ETag) for Optimistic Concurrency Control and Replay Protection)** — **RESOLVED**.
  `ScimMeta` in `crates/core/src/enterprise/scim.rs:33–50` now includes `pub version: Option<String>` and builder method `with_version(mut self, version: impl Into<String>)`. Additionally, `ScimGroup` (`scim.rs:117, 128–131`) now includes `pub meta: Option<ScimMeta>`, populated by default with `resource_type: "Group"`.

- **v03 Security M5 (Missing Session ID Rotation (Regeneration) Exposes Applications to Session Fixation Attacks)** — **RESOLVED**.
  `SessionStore` in `crates/core/src/auth/session.rs:85–103` now defines `regenerate_id<'a>(&'a self, old_id: &'a str) -> BoxFuture<'a, Result<Session, AuthError>>` with a default implementation that loads the session, generates a fresh UUID v4 identifier, and re-persists the session data under the new key. (Note: Non-atomic destroy-before-save sequencing is tracked in finding m1).

- **v03 Security M6 (`SessionScheme` Silently Degrades Authenticated Users to Guests on Session Store Failures)** — **RESOLVED**.
  In `crates/core/src/auth/schemes.rs:194–213`, `SessionScheme` replaces `if let Ok(Some(session))` with an explicit `match self.store.load(val).await`. While `Ok(None)` cleanly returns `Ok(None)` to allow multi-scheme fallback on expired cookies, `Err(err)` immediately returns `Err(err)`, surfacing backend storage timeouts (e.g. Redis connection errors) as HTTP 500 internal server errors rather than silently treating authenticated users as anonymous guests.

- **v03 Security m1 (Unvalidated Reverse Proxy Trust for `x-forwarded-proto` and Axum Relative URI Blindness in `SecurityHeadersService`)** — **RESOLVED**.
  `SecurityHeadersConfig` in `crates/core/src/security/headers.rs:38–44, 95–105` added `native_tls: bool` (for in-process TLS termination) and `trust_proxy_headers: bool` (defaulting to `true`, configurable to `false` when running without trusted reverse proxies). In addition, RFC 7239 `Forwarded: proto=https` header inspection was added alongside `x-forwarded-proto`.

- **v03 Security m2 (Substring Heuristic Blind Spots and Leaky URL Parsing in `crates/core/src/env/secrets.rs`)** — **RESOLVED**.
  In `crates/core/src/env/secrets.rs:22–52`, `is_secret_key` was expanded to cover `APIKEY`, `AUTHORIZATION`, `BEARER`, `SESSION`, and `SESSION_ID` in both substring and exact matches. In `redact_credential_urls_in_text` (`secrets.rs:97`), `authority.find('@')` was corrected to `authority.rfind('@')`, properly isolating user credentials when passwords contain `@` characters.

- **v03 Security m3 (SAML Parser Comment Truncation and Tag Extraction Fragility)** — **RESOLVED**.
  `strip_comments()` in `crates/core/src/enterprise/saml.rs:76–89` now returns `Result<String, SamlError>`, returning `Err(SamlError::Malformed("unclosed XML comment".into()))` instead of truncating remaining XML. In addition, `extract_attribute()` (`saml.rs:141–157`) supports single quotes, double quotes, and arbitrary whitespace around the equals sign (`attr="`, `attr='`, `attr ="`, `attr = "`, `attr= "`).

---

## Findings

### M1 SAML 2.0 Signature Reference binding in `SamlParser` rejects standard signed Response envelopes and is fragile to namespace variants

- **Where:** `crates/core/src/enterprise/saml.rs:263–287`
- **What it says:**
  ```rust
  // Optional Assertion ID bound to the verified assertion subtree
  let id = if let Some(pos) = assertion_xml.find("ID=\"") {
      let rest = &assertion_xml[pos + 4..];
      if let Some(end) = rest.find('"') {
          rest[..end].to_string()
      } else {
          "assertion".to_string()
      }
  } else {
      "assertion".to_string()
  };

  // Security check: Signature Reference URI must match Assertion ID if present
  let ref_pattern = "<ds:Reference URI=\"#";
  if let Some(pos) = xml.find(ref_pattern) {
      let rest = &xml[pos + ref_pattern.len()..];
      if let Some(end) = rest.find('"') {
          let ref_id = &rest[..end];
          if ref_id != id {
              return Err(SamlError::SignatureWrappingDetected(format!(
                  "signature reference URI '#{ref_id}' does not match assertion ID '{id}'"
              )));
          }
      }
  }
  ```
- **Why it's a problem:**
  1. **False positive rejection of standard signed SAML Responses:** In the SAML 2.0 Web Browser SSO Profile (OASIS SAML 2.0 Core Section 5), Identity Providers (such as Microsoft Entra / Azure AD, Okta, PingFederate, and ADFS) frequently sign the root `<samlp:Response>` envelope rather than (or in addition to) the child `<saml2:Assertion>`. In a signed response, `<ds:Signature>` is a child of `<samlp:Response ID="_resp_123">`, and `<ds:Reference URI="#_resp_123">` references the Response element, while the enclosed assertion has `ID="_assert_456"`. Because `SamlParser` only extracts the Assertion ID and asserts `ref_id == id`, it rejects all signed Response envelopes with `SignatureWrappingDetected("signature reference URI '#_resp_123' does not match assertion ID '_assert_456'")`. Legitimate enterprise users are completely locked out of authentication.
  2. **Fragility and verification bypass:**
     - The check hardcodes `let ref_pattern = "<ds:Reference URI=\"#";`. If an Identity Provider outputs single quotes (`URI='#id'`), whitespace (`URI = "#id"`), or uses the default XML-DSig namespace without prefix (`<Reference URI="#id">`), `xml.find()` returns `None`, silently bypassing the XSW cryptographic binding check entirely.
     - The assertion ID extractor hardcodes `find("ID=\"")` instead of using the newly added `extract_attribute()`. If an IdP formats the attribute as `ID='_assert_456'` or `ID = "_assert_456"`, `id` defaults to `"assertion"`. Even a legitimately signed assertion with `<ds:Reference URI="#_assert_456">` will then be rejected because `"_assert_456" != "assertion"`.
- **Suggested fix:**
  1. Extract both the Response ID (from the root element) and the Assertion ID using `extract_attribute`.
  2. When `<ds:Reference URI="#...">` is present, verify that `ref_id` matches *either* the Assertion ID or the enclosing Response ID.
  3. Tokenize or search for `<Reference` elements across arbitrary namespace prefixes and support single quotes and whitespace (or leverage an XML parser like `quick-xml` for AST node traversal).

---

### M2 Insecure defaults in `CsrfConfig` persist: missing mandatory HMAC secret and insecure cookies by default

- **Where:** `crates/core/src/security/csrf.rs:64–77, 167–172`
- **What it says:**
  ```rust
  impl Default for CsrfConfig {
      fn default() -> Self {
          Self {
              header_name: "x-csrf-token".to_string(),
              cookie_name: "csrf_token".to_string(),
              exempt_prefixes: BTreeSet::new(),
              secret: None,
              auto_inject_cookie: false,
              cookie_path: "/".to_string(),
              cookie_same_site: "Strict".to_string(),
              cookie_secure: false,
              cookie_http_only: false,
          }
      }
  }

  pub fn verify_token(&self, token: &str) -> bool {
      let Some(secret) = &self.secret else {
          return !token.is_empty();
      };
      ...
  ```
- **Why it's a problem:**
  While multi-cookie traversal was implemented in `CsrfService::call` (resolving v03 M2), the root cause identified in v03 M1 was left untouched. When an application enables CSRF protection using `CsrfLayer::new()`:
  1. `secret` is `None`. Under this default, `verify_token` accepts *any* non-empty string. If an attacker controls an insecure sibling subdomain (or an attacker on the same local network over plain HTTP) injects a cookie `csrf_token=evil_token` scoped to the parent domain, a cross-origin request carrying `x-csrf-token: evil_token` satisfies `verify_token` and `safe_equal`. The double-submit check passes, completely bypassing CSRF protection out of the box.
  2. `cookie_secure` is `false`, allowing CSRF cookies to be transmitted over cleartext HTTP connections.
- **Suggested fix:**
  1. Provide secure defaults: either generate an ephemeral 32-byte HMAC secret upon `CsrfConfig::default()` or require secret initialization via `CsrfConfig::with_secret(...)` / `CsrfLayer::with_secret(...)`.
  2. Default `cookie_secure: true` (or toggle automatically based on secure transport / test environment flags).
  3. Emit a `tracing::warn!` diagnostic warning on application startup if `CsrfLayer` is active with `secret: None`.

---

### m1 Non-atomic session ID rotation in `SessionStore::regenerate_id` risks state loss on write failures

- **Where:** `crates/core/src/auth/session.rs:86–103`
- **What it says:**
  ```rust
  fn regenerate_id<'a>(&'a self, old_id: &'a str) -> BoxFuture<'a, Result<Session, AuthError>> {
      Box::pin(async move {
          let existing = self.load(old_id).await?.ok_or_else(|| {
              AuthError::InvalidCredentials("cannot regenerate nonexistent session".into())
          })?;
          self.destroy(old_id).await?;
          let new_session = Session {
              id: uuid::Uuid::new_v4().to_string(),
              user_id: existing.user_id,
              roles: existing.roles,
              permissions: existing.permissions,
              data: existing.data,
              expires_at: existing.expires_at,
          };
          self.save(new_session.clone()).await?;
          Ok(new_session)
      })
  }
  ```
- **Why it's a problem:**
  `self.destroy(old_id)` is awaited *before* `self.save(new_session)`. If saving the newly generated session fails (e.g. due to a transient Redis network timeout, pool saturation, or payload serialization error), the old session has already been permanently deleted from storage, and the new session was never committed. The user's active session data is irrecoverably destroyed, causing an immediate unexpected logout and data loss.
- **Suggested fix:**
  Invert the execution order: persist the new session first, and delete the old session only after the new session has been saved successfully:
  ```rust
  self.save(new_session.clone()).await?;
  let _ = self.destroy(old_id).await;
  Ok(new_session)
  ```
  In Redis-backed stores (`RedisSessionStore`), override `regenerate_id` using atomic Redis `RENAME` or a transactional pipeline.

---

### m2 Audit logger SQL literal escaping causes data distortion in PostgreSQL and SQLite

- **Where:** `crates/core/src/security/audit.rs:158–174`
- **What it says:**
  ```rust
  fn escape_sql_literal(s: &str) -> String {
      let mut out = String::with_capacity(s.len() + 8);
      out.push('\'');
      for c in s.chars() {
          match c {
              '\'' => out.push_str("''"),
              '\\' => out.push_str("\\\\"),
              '\0' => out.push_str("\\0"),
              '\n' => out.push_str("\\n"),
              '\r' => out.push_str("\\r"),
              '\x1a' => out.push_str("\\Z"),
              other => out.push(other),
          }
      }
      out.push('\'');
      out
  }
  ```
- **Why it's a problem:**
  `escape_sql_literal` applies MySQL C-string escape rules (`\\`, `\0`, `\n`, `\r`, `\Z`) unconditionally across all persistence backends. In standard SQL (PostgreSQL with default `standard_conforming_strings=on` and SQLite), backslashes are ordinary characters inside single-quoted string literals and are not treated as escape delimiters. As a result, any legitimate backslash in user IDs, metadata, or paths is doubled upon insertion (`corp\user` is stored as `corp\\user`), and newlines become the literal two-character string `\n` rather than an actual line break. While this successfully eliminates SQL injection on MySQL, it alters and corrupts audit record data when running against PostgreSQL or SQLite.
- **Suggested fix:**
  Parameterize `DatabaseBackend::execute` with driver-level parameter binding (`$1..$N` in Postgres/SQLite, `?` in MySQL) rather than raw string formatting, or adjust `escape_sql_literal` behavior based on the active backend dialect.

---

## Positive notes

- **Strict SAML subtree claim isolation:** All assertion identity attributes (`Issuer`, `Subject`, `Audience`, `Conditions`, and `AttributeStatement`) are now extracted exclusively from within the verified assertion byte slice (`assertion_xml`), eliminating Type-1 Envelope Injection XSW attacks.
- **Dynamic namespace prefix scanning:** `SamlParser` scans for all variants of `<([a-zA-Z0-9_\-]+:)?Assertion` and rejects payloads with multiple assertion elements, successfully closing Type-2 prefix evasion.
- **Deprecation of unverified SAML parsing:** Direct unverified parsing in production is deprecated, and `SamlParser::parse_signed` establishes a clean, explicit contract requiring an injected cryptographic signature verifier.
- **Elimination of MySQL SQL injection in audit logging:** `escape_sql_literal` comprehensively handles backslashes and single quotes, completely blocking escape break-outs in MySQL while preserving the caller's correlation ID `record.id`.
- **Automated metadata secret redaction:** Passing all audit record metadata through `redact_value` before JSON serialization prevents credentials, tokens, and database passwords from being stored in plaintext in `rume_audit_log`.
- **Multi-cookie traversal in CSRF service:** `CsrfService` iterates across all matching cookies in the `Cookie` header, allowing legitimate host-scoped tokens to validate even in the presence of invalid tossed cookies from subdomains.
- **SCIM 2.0 optimistic concurrency support:** `ScimMeta` and `ScimGroup` now support `version` for RFC 7643/7644 ETag validation and concurrency control.
- **Resilient session error propagation:** `SessionScheme` no longer converts backend infrastructure failures (e.g. Redis timeouts) into unauthenticated guest sessions, preserving security boundaries and generating proper HTTP 500 error responses.
- **Reverse proxy trust controls:** `SecurityHeadersConfig` cleanly distinguishes between native in-process TLS and reverse proxy headers, adding support for RFC 7239 `Forwarded: proto=https` and configurable proxy trust.
- `#![forbid(unsafe_code)]` remains strictly enforced across all security and authentication modules.

---

## Verdict

**Fail** (Must land fix pass first).

The security protocols and enterprise subsystem between `cf82559` and `HEAD` successfully resolves 9 of the 12 prior findings, including critical SQL injection and SAML unverified bypass vulnerabilities. However, this change cannot be accepted as-is due to two Major defects:
1. **Finding M1**: `SamlParser::parse_with_verifier` enforces a rigid assertion ID check on digital signature reference URIs, causing standard SAML responses where the `<Response>` envelope is signed (standard in Okta, Azure AD, and ADFS) to fail with `SignatureWrappingDetected`, while remaining fragile against quote and namespace variants.
2. **Finding M2**: `CsrfConfig` continues to default to unauthenticated double-submit tokens (`secret: None`) and insecure cookies (`cookie_secure: false`), leaving default deployments vulnerable to subdomain cookie-tossing.

A fix pass addressing findings **M1** and **M2** must land before this diff can be merged to production.
