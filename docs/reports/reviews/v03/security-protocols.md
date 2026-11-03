# Security, SAML & Protocol Hardening Review — v03

## Summary

The Rume security, authentication, and protocol hardening subsystems provide essential defenses including double-submit CSRF protection, declarative authorization policies, hardened HTTP security headers, and structured audit logging. However, the subsystem is **not ready for production** due to severe cryptographic and protocol vulnerabilities: the SAML 2.0 parser relies on unanchored substring searches without AST verification, leaving it wide open to XML Signature Wrapping (XSW) attacks; `SamlParser::parse()` permits unverified assertion consumption in production; `SqlAuditLogger` constructs unparameterized queries vulnerable to SQL injection on MySQL; CSRF protection defaults to unsigned tokens vulnerable to subdomain tossing and first-cookie selection DoS; and session management lacks session fixation protections while silently degrading authenticated users to guests on backend timeouts.

---

## Prior decisions

- **v01 S-M1 (Path traversal in `file:` env loading)** — **REAFFIRMED**. Verified in `rume/src/env/loading.rs:254–267`. `resolve_value` canonicalizes both root dir and resolved path, rejecting escapes with `AppError::PermissionDenied`.
- **v01 S-M2 (Secrets held as plain `String` in `ResolvedEnv`)** — **REAFFIRMED**. Verified in `rume/src/env/loading.rs:27–36, 174–188`. Secrets are encapsulated inside `secrecy::SecretBox<String>` and scrubbed from general debug representations.
- **v01 S-M3 (CLI commands print secrets to terminal in plaintext)** — **REAFFIRMED**. Verified in `rume-cli/src/commands/env/check.rs:47–51` and `rume-cli/src/commands/config/check.rs:38–48`. Secrets and database URLs require `--unredacted` to display.
- **v01 S-M4 (`AppError` renders raw `to_string()` into client JSON)** — **REAFFIRMED**. Verified in `rume/src/error.rs:173–275`. RFC 9457 Problem Details (`application/problem+json`) sanitize client responses while retaining server diagnostics in logs.
- **v01 S-m1 (`redact()` heuristic false negatives)** — **REAFFIRMED**. Broadened in `rume/src/env/secrets.rs:18–42` to cover `_PASS`, `_PWD`, `_CRED`, `_KEY`, and `STRIPE_*`. Further heuristic extensions evaluated in finding m2.
- **v01 S-m2 (`/health/ready` exposes pid and platform in `debug_info`)** — **REAFFIRMED**. Verified in `rume/src/routing/health.rs:289–294`. Returns an empty `BTreeMap`.
- **v01 S-m3 (`Rejection` echoes parameter values and names in error response)** — **REAFFIRMED**. Verified in `rume/src/routing/rejection.rs:19–42`. Rejections convert to generic Problem Details.
- **v02 Security C1 (SAML assertion parser hardcodes expiration timestamps and lacks signature verification)** — **CHALLENGED (Defective Remediation & Incomplete Signature Binding)**. While `NotBefore` / `NotOnOrAfter` parsing was added alongside a `SamlSignatureVerifier` trait, the remediation is structurally incomplete: `SamlParser` continues to use naive substring searches rather than a verified XML DOM tree, leaves assertion data unanchored from the verified signature element, fails against prefix-evading XML Signature Wrapping (XSW) attacks, and retains `parse()` as an unverified public bypass (findings C1, C2).
- **v02 Security C2 (Multi-tenant cache key delimiter collision permits cross-tenant cache poisoning)** — **REAFFIRMED**. Validated in `rume/src/tenant/cache.rs` where tenant IDs and cache keys disallow delimiter colons or enforce length-prefixed key boundaries (audited in Lens 4).
- **v02 Security M1 (Subdomain tenant resolver permits suffix-matching domain spoofing)** — **REAFFIRMED**. Validated in `rume/src/tenant/resolver.rs:133–149` where `base_domain` normalization requires a leading dot (audited in Lens 4).
- **v02 Security M2 (`HeaderTenantResolver` shares a single map for IDs and slugs)** — **REAFFIRMED**. Validated in `rume/src/tenant/resolver.rs:49–56` where IDs and slugs are indexed into distinct lookup structures (audited in Lens 4).
- **v02 Security M3 (`ApiKeyScheme` stores plaintext API keys in memory with early-exit linear scan)** — **REAFFIRMED**. Verified in `rume/src/auth/schemes.rs:86–114, 139–148`. Keys are hashed via SHA-256 upon registration into `HashMap<[u8; 32], AuthUser>` and looked up in O(1) time without linear scan timing side-channels.
- **v02 Security M4 (Double-submit CSRF layer lacks token issuance and is vulnerable to subdomain tossing)** — **CHALLENGED (Defective Remediation & Insecure Defaults)**. Token issuance and HMAC signing were added, but `CsrfConfig::default()` disables HMAC signing (`secret: None`), disables `cookie_secure`, and disables auto-injection by default. Naive double-submit without HMAC remains active by default, and cookie parsing suffers from first-cookie bias enabling denial-of-service via tossed cookies (findings M1, M2).
- **v02 Security m1 (`AuthError::ProviderError` maps to HTTP 400 Bad Request instead of HTTP 500)** — **REAFFIRMED**. Verified in `rume/src/auth/error.rs:38`. `ProviderError` maps to `AppError::internal`, returning HTTP 500 without blaming the client.
- **v02 Security m2 (Stale session cookie in `SessionScheme` immediately aborts authentication chain)** — **REAFFIRMED**. Verified in `rume/src/auth/schemes.rs:194–203`. Stale/missing cookies return `Ok(None)`, enabling fallback schemes in multi-scheme setups.
- **v02 Security m3 (`SecurityHeadersLayer` injects HSTS header unconditionally over plain HTTP)** — **CHALLENGED (Partial Remediation & Proxy Header Spoofing Risk)**. `hsts_require_tls` was added, but the middleware blindly trusts `x-forwarded-proto: https` from unverified upstream network connections and fails to recognize native TLS termination in Hyper/Axum due to relative request URIs (finding m1).
- **Persistent Structured Audit Logging (`rume/src/security/audit.rs`)** — **NOTE-NEW**. Evaluated in C3 and M3.
- **Enterprise SCIM 2.0 Concurrency & Versioning (`rume/src/enterprise/scim.rs`)** — **NOTE-NEW**. Evaluated in M4.
- **Session Lifecycle & Management (`rume/src/auth/session.rs`, `redis.rs`)** — **NOTE-NEW**. Evaluated in M5 and M6.

---

## Findings

### C1 SAML 2.0 Parser Vulnerable to XML Signature Wrapping (XSW) Attacks via Unbound Global Tag Extraction and Namespace Prefix Evasion

- **Where:** `rume/src/enterprise/saml.rs:182–258`
- **What it says:**
  ```rust
  pub fn parse_with_verifier(
      xml: &str,
      verifier: Option<&dyn SamlSignatureVerifier>,
  ) -> Result<SamlAssertion, SamlError> {
      if let Some(v) = verifier {
          v.verify_signature(xml)?;
      }

      let clean_xml = strip_comments(xml);
      let xml = clean_xml.as_str();

      // Security check: Guard against XML Signature Wrapping (XSW) attacks
      // Reject messages with multiple assertion tags.
      let assertion_count = xml.match_indices("<saml2:Assertion").count()
          + xml.match_indices("<saml:Assertion").count()
          + xml.match_indices("<Assertion").count();
      if assertion_count > 1 {
          return Err(SamlError::SignatureWrappingDetected(format!(
              "multiple ({assertion_count}) Assertion elements detected; possible XML signature wrapping attack"
          )));
      }
      ...
      // Find Subject NameID
      let subject = Self::extract_tag(xml, "saml:NameID")
          .or_else(|| Self::extract_tag(xml, "saml2:NameID"))
          .or_else(|| Self::extract_tag(xml, "NameID"))
          .ok_or_else(|| SamlError::Malformed("missing NameID in Subject".into()))?
          .to_string();
  ```
- **Why it's a problem:**
  The parser attempts to prevent XML Signature Wrapping (XSW) attacks using naive string counting (`assertion_count`), while extracting identity claims (`Issuer`, `Subject`, `Audience`, `Attributes`) via global unanchored substring searches on `clean_xml` starting from byte 0. This architecture suffers from three fatal cryptographic bypasses:
  1. **Envelope Injection (Type-1 XSW)**: In standard SAML responses, the signed `<saml2:Assertion>` is wrapped inside a `<samlp:Response>` envelope. An attacker can inject a forged `<saml2:Subject><saml2:NameID>admin@corp.com</saml2:NameID></saml2:Subject>` inside the `<samlp:Response>` envelope *before* the legitimate signed assertion. Because there is only one `<saml2:Assertion>` element, `assertion_count` evaluates to 1. The verifier checks the signature over the legitimate assertion and succeeds. However, `Self::extract_tag(xml, "saml2:NameID")` scans the entire document from byte 0 and extracts `admin@corp.com` from the unsigned envelope. The attacker is authenticated as `admin@corp.com`.
  2. **Namespace Prefix Evasion (Type-2 XSW)**: XML namespaces permit arbitrary prefixes (e.g. `xmlns:authn="urn:oasis:names:tc:SAML:2.0:assertion"`). `assertion_count` only counts `<saml2:Assertion`, `<saml:Assertion`, and `<Assertion`. An attacker can supply a signed `<authn:Assertion ID="_valid">` alongside an unsigned forged `<saml2:Assertion ID="_forged">`. `assertion_count` evaluates to 1 because `<authn:Assertion>` is ignored by the string counter. `extract_tag` then extracts claims from the forged `<saml2:Assertion>`.
  3. **Zero Cryptographic Binding**: `verifier.verify_signature(xml)` accepts the entire raw string and returns `()`. The parser has no programmatic reference to the specific XML node, element ID, or canonicalized byte range that passed digital signature verification.
- **Suggested fix:**
  Replace naive string searching with a robust XML parser (such as `quick-xml` or an XML DOM parser). Cryptographically bind assertion consumption to signature verification:
  1. The signature verifier must validate the digest over the exact element referenced in `<ds:Reference URI="#id">`.
  2. The parser must only extract `Subject`, `Issuer`, `Conditions`, and `Attributes` from the sub-tree rooted at that validated element.
  3. Reject any document containing unrecognized envelope assertions or unsigned assertion tags regardless of namespace prefix.

---

### C2 Public `SamlParser::parse()` Bypasses Cryptographic Verification in Production

- **Where:** `rume/src/enterprise/saml.rs:170–172, 182–190`
- **What it says:**
  ```rust
  /// Parses an XML string containing a SAML 2.0 Assertion.
  pub fn parse(xml: &str) -> Result<SamlAssertion, SamlError> {
      Self::parse_with_verifier(xml, None)
  }
  ```
  And in `SamlError` (lines 29–31):
  ```rust
  /// Missing mandatory digital signature verifier.
  #[error("signature verification required: missing signature verifier")]
  MissingSignatureVerifier,
  ```
- **Why it's a problem:**
  `SamlParser::parse()` is exposed as a primary public API for SAML assertion parsing, but internally passes `verifier = None`. In production code, any developer or integration handler calling `SamlParser::parse(&xml_payload)` will successfully parse and validate unverified SAML assertions without cryptographic XML-DSig verification against the Identity Provider's public certificate. Furthermore, `SamlError::MissingSignatureVerifier` is defined as dead code and never returned. This provides an accidental trapdoor where unauthenticated, spoofed XML payloads can be consumed as valid identities.
- **Suggested fix:**
  Enforce mandatory signature verification by default:
  1. Deprecate or remove `SamlParser::parse(xml)`.
  2. Make `SamlParser::parse_signed(xml, verifier)` or `SamlParser::parse_verified(xml, &config)` the sole production entry point.
  3. If unverified parsing is required for diagnostic or offline testing purposes, isolate it into an explicitly named method: `SamlParser::parse_unverified_insecure_for_testing(xml)`.

---

### C3 SQL Injection Vulnerability in `SqlAuditLogger::record()` via String Formatting and Incomplete MySQL Escape Handling

- **Where:** `rume/src/security/audit.rs:161, 173–188`
- **What it says:**
  ```rust
  let id = uuid::Uuid::new_v4().to_string();
  ...
  let actor = record.actor_id.as_deref().unwrap_or("NULL");
  let target = record.target_id.as_deref().unwrap_or("NULL");
  let ip = record.ip_address.as_deref().unwrap_or("NULL");

  let actor_val = if actor == "NULL" { "NULL".to_string() } else { format!("'{}'", actor.replace('\'', "''")) };
  let target_val = if target == "NULL" { "NULL".to_string() } else { format!("'{}'", target.replace('\'', "''")) };
  let ip_val = if ip == "NULL" { "NULL".to_string() } else { format!("'{}'", ip.replace('\'', "''")) };
  let meta_val = format!("'{}'", meta_json.replace('\'', "''"));

  let stmt = format!(
      "INSERT INTO rume_audit_log (id, action, resource, status, actor_id, target_id, ip_address, metadata, created_at) VALUES ('{id}', '{}', '{}', '{status_str}', {actor_val}, {target_val}, {ip_val}, {meta_val}, {})",
      record.action.replace('\'', "''"),
      record.target_type.replace('\'', "''"),
      record.timestamp_ms
  );

  self.backend
      .execute(&stmt)
      .await
      .map_err(|e| AuditError::Sink(e.to_string()))?;
  ```
- **Why it's a problem:**
  1. **SQL Injection on MySQL**: SQL statements are constructed via manual `format!()` string concatenation with simple quote doubling (`.replace('\'', "''")`). In MySQL, backslash (`\`) is an escape character by default unless `NO_BACKSLASH_ESCAPES` is set. If an attacker's username, actor ID, or metadata ends with a backslash (e.g. `corp\user\`), the formatted query becomes `VALUES ('id', 'action', 'user\', ...)`. The trailing backslash escapes the closing quote, breaking out of the string literal and executing subsequent query tokens as arbitrary SQL commands.
  2. **Correlation ID Overwrite**: `AuditRecord::new()` generates a UUID v4 in `record.id`. However, line 161 generates a separate `let id = uuid::Uuid::new_v4().to_string();` and inserts that instead, completely discarding the caller's `record.id`. Any application component attempting to correlate client logs with audit database records via `record.id` will fail to locate the record.
- **Suggested fix:**
  1. Parameterize `DatabaseBackend` statement execution to pass bound parameters (`$1..$N` in Postgres/SQLite, `?` in MySQL) rather than interpolating strings into raw SQL.
  2. If raw SQL string construction is unavoidable as an intermediate step, strictly escape both backslashes and single quotes (`.replace('\\', "\\\\").replace('\'', "''")`).
  3. Retain `record.id` in `stmt` instead of generating a new throwaway UUID.

---

### M1 Insecure Defaults in `CsrfConfig` Permit Subdomain Cookie-Tossing and Lack Mandatory HMAC Signing

- **Where:** `rume/src/security/csrf.rs:64–77, 159–163`
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
  When developers instantiate `CsrfLayer::new()` without manual customization, `secret` is `None` and `cookie_secure` is `false`. Under this configuration, `verify_token` accepts *any* non-empty string. If an attacker controls an insecure sibling subdomain (or an attacker on the same local network over plain HTTP) injects a cookie `csrf_token=evil_token` scoped to the parent domain, a cross-origin request carrying `x-csrf-token: evil_token` satisfies `verify_token` and `safe_equal`. The double-submit validation succeeds, completely bypassing CSRF protection out of the box.
- **Suggested fix:**
  1. Default `cookie_secure: true` when running outside test mode, or warn in logs when `cookie_secure: false` is used.
  2. Support the `__Host-` cookie prefix by default (e.g. `__Host-csrf_token`) which modern browsers prevent subdomains from tossing or overwriting.
  3. Require or strongly recommend an HMAC secret key in `CsrfConfig` so that unsigned tossed tokens fail validation automatically.

---

### M2 Subdomain Cookie-Tossing Causes Denial of Service via First-Cookie Selection Bias in `CsrfService`

- **Where:** `rume/src/security/csrf.rs:271–280, 332–342`
- **What it says:**
  ```rust
  let cookie_token = req
      .headers()
      .get(COOKIE)
      .and_then(|val| val.to_str().ok())
      .and_then(|cookies| {
          cookies.split(';').find_map(|pair| {
              let (name, value) = pair.trim().split_once('=')?;
              if name == config.cookie_name {
                  Some(value)
              } else {
                  None
              }
          })
      });
  ```
- **Why it's a problem:**
  HTTP request headers can contain multiple cookies with the same name if a cookie was set on both a parent domain and a subdomain (e.g. `Cookie: csrf_token=tossed_bad_cookie; csrf_token=legitimate_signed_cookie`). `find_map` returns the *first* matching cookie encountered. If an attacker on a sibling subdomain tosses an invalid cookie `csrf_token=invalid` with domain `.example.com`, that cookie often appears first in the `Cookie` header. `verify_token` rejects the tossed token with `403 CSRF_TOKEN_INVALID`, permanently locking the victim out of performing any mutating HTTP requests across the application.
- **Suggested fix:**
  1. When searching for `config.cookie_name`, do not abort on the first match; iterate through all cookies sharing that name and accept the request if *any* matching cookie possesses a valid HMAC signature and matches the incoming header token.
  2. Adopt the `__Host-` cookie prefix, which prevents parent domains and sibling subdomains from injecting cookies into the host's scope.

---

### M3 Lack of Secret Redaction in Audit Logging Metadata

- **Where:** `rume/src/security/audit.rs:167–168`
- **What it says:**
  ```rust
  let meta_json = serde_json::to_string(&record.metadata)
      .map_err(|e| AuditError::Sink(e.to_string()))?;
  ```
- **Why it's a problem:**
  Audit records frequently log contextual security events, including failed login attempts, API key revocations, webhook deliveries, and permission rejections. When callers include request headers, database connection parameters, or diagnostic error messages in `record.metadata`, `serde_json::to_string` serializes the raw map into JSON without any redaction pass. Sensitive credentials (such as database passwords, OAuth tokens, bearer tokens, or API keys) are stored in plaintext in the persistent `rume_audit_log` table, violating SOC 2 / HIPAA compliance standards and expanding the breach surface.
- **Suggested fix:**
  Before serializing `record.metadata`, pass each key and value through `crate::env::secrets::redact_value(key, val)` and scrub credential URLs via `redact_credential_urls_in_text(val)`.

---

### M4 SCIM 2.0 Entities Lack `meta.version` (ETag) for Optimistic Concurrency Control and Replay Protection

- **Where:** `rume/src/enterprise/scim.rs:36–41, 72–75`
- **What it says:**
  ```rust
  /// SCIM Metadata sub-attribute.
  #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
  #[serde(rename_all = "camelCase")]
  pub struct ScimMeta {
      pub resource_type: String,
      pub created: Option<String>,
      pub last_modified: Option<String>,
      pub location: Option<String>,
  }
  ```
- **Why it's a problem:**
  Under RFC 7643 Section 3.1 and RFC 7644 Section 3.14, SCIM service providers MUST support optimistic concurrency control via ETags on resource metadata (`meta.version`). Enterprise Identity Providers (such as Okta, Azure AD, or PingFederate) send `If-Match: W/"<version>"` on `PUT` and `PATCH` requests to prevent conflicting updates and replay overwrites during concurrent provisioning operations. The absence of `version` in `ScimMeta` causes IdP synchronization errors and leaves user deprovisioning requests vulnerable to out-of-order race conditions.
- **Suggested fix:**
  Add `pub version: Option<String>` to `ScimMeta` and provide helpers to compute weak or strong ETags (e.g. SHA-256 hash of resource contents or monotonic version sequence) for `ScimUser` and `ScimGroup`.

---

### M5 Missing Session ID Rotation (Regeneration) Exposes Applications to Session Fixation Attacks

- **Where:** `rume/src/auth/session.rs:74–84`, `rume/src/auth/redis.rs:84–111`
- **What it says:**
  ```rust
  pub trait SessionStore: Send + Sync + 'static {
      fn load(&self, id: &str) -> BoxFuture<'_, Result<Option<Session>, AuthError>>;
      fn save(&self, session: Session) -> BoxFuture<'_, Result<(), AuthError>>;
      fn destroy(&self, id: &str) -> BoxFuture<'_, Result<(), AuthError>>;
  }
  ```
- **Why it's a problem:**
  `SessionStore` only provides `load`, `save`, and `destroy`. There is no mechanism or API to atomically rotate or regenerate a session identifier (`regenerate_id`) while preserving existing session data. In web authentication workflows, if a guest session is elevated to an authenticated user without rotating the session identifier, an attacker who pre-planted a session identifier in the victim's browser (e.g. via cookie injection or link sharing) gains authenticated access to the victim's account (OWASP Session Fixation).
- **Suggested fix:**
  Add a `regenerate(&self, old_id: &str, new_id: &str) -> BoxFuture<'_, Result<(), AuthError>>` method to `SessionStore` (implemented atomically via Redis `RENAME` or transaction), and provide a convenience helper `Session::regenerate()` in `rume::auth`.

---

### M6 `SessionScheme` Silently Degrades Authenticated Users to Guests on Session Store Failures

- **Where:** `rume/src/auth/schemes.rs:194–203`
- **What it says:**
  ```rust
  if let Ok(Some(session)) = self.store.load(val).await {
      let mut user = AuthUser::new(session.user_id)
          .with_roles(session.roles)
          .with_permissions(session.permissions);
      user.metadata = session.data;
      return Ok(Some(user));
  }
  // Stale or expired cookie: allow fallback to subsequent schemes in the chain
  return Ok(None);
  ```
- **Why it's a problem:**
  When `self.store.load(val).await` fails due to an infrastructure fault (e.g. Redis connection timeout, pool exhaustion, network partition, or malformed JSON), `load` returns `Err(AuthError::ProviderError(...))`. The `if let Ok(Some(session))` check fails, and execution falls through to line 202: `return Ok(None)`. Instead of propagating the internal server error (HTTP 500) to notify operators of backend failure, the middleware silently treats the user as an unauthenticated guest. On routes supporting optional authentication, this silently executes privileged actions as anonymous requests or corrupts user state without raising alerts.
- **Suggested fix:**
  Inspect the `Result` from `self.store.load(val).await`:
  - `Ok(Some(session))` -> return `Ok(Some(user))`
  - `Ok(None)` -> return `Ok(None)` (stale/expired cookie fallback)
  - `Err(err)` -> return `Err(err)` (propagate infrastructure failure as HTTP 500)

---

### m1 Unvalidated Reverse Proxy Trust for `x-forwarded-proto` and Axum Relative URI Blindness in `SecurityHeadersService`

- **Where:** `rume/src/security/headers.rs:156–163`
- **What it says:**
  ```rust
  let is_https = req.uri().scheme() == Some(&axum::http::uri::Scheme::HTTPS)
      || req
          .headers()
          .get("x-forwarded-proto")
          .and_then(|v| v.to_str().ok())
          .map(|v| v.eq_ignore_ascii_case("https"))
          .unwrap_or(false);
  ```
- **Why it's a problem:**
  1. Any client connecting directly over plain HTTP can inject `x-forwarded-proto: https`. The server blindly trusts this header without verifying that the request originated from a trusted reverse proxy, causing Rume to emit `Strict-Transport-Security` over plain HTTP in violation of RFC 6797 Section 7.2.
  2. Incoming HTTP requests in Axum/Hyper contain relative URIs (e.g. `/path`), where `req.uri().scheme()` is virtually always `None`. If Rume terminates TLS natively in-process (e.g. via `axum-server::bind_rustls`) without a reverse proxy injecting `x-forwarded-proto`, `is_https` evaluates to `false`, and HSTS headers are omitted unless developers manually disable `hsts_require_tls`.
- **Suggested fix:**
  1. Add a trusted proxy IP configuration to `SecurityHeadersConfig`, ignoring `x-forwarded-proto` unless the request peer IP matches the trusted proxy range.
  2. Support the standard RFC 7239 `Forwarded: proto=https` header.
  3. Provide a way to flag native TLS termination on `SecurityHeadersConfig` (e.g. `with_native_tls(true)`).

---

### m2 Substring Heuristic Blind Spots and Leaky URL Parsing in `rume/src/env/secrets.rs`

- **Where:** `rume/src/env/secrets.rs:18–42, 87–97`
- **What it says:**
  In `is_secret_key`:
  ```rust
  if upper.contains("SECRET")
      || upper.contains("PASSWORD")
      || upper.contains("TOKEN")
      || upper.contains("_PASS")
      || upper.contains("_PWD")
      || upper.contains("_CRED")
      || upper.contains("_KEY")
  ```
  In `redact_credential_urls_in_text`:
  ```rust
  if let Some(at_pos) = authority.find('@') {
      let user_info = &authority[..at_pos];
      if let Some(colon_pos) = user_info.find(':') {
          let user = &user_info[..colon_pos];
          let host_and_rest = &authority[at_pos..];
          out.push_str(user);
          out.push_str(":[redacted]");
          out.push_str(host_and_rest);
  ```
- **Why it's a problem:**
  1. `is_secret_key` misses standalone variable names and conventions such as `APIKEY` (without underscore), `AUTHORIZATION`, `BEARER`, `SESSION_ID`, and `SESSION`.
  2. In `redact_credential_urls_in_text`, `authority.find('@')` matches the *first* `@` rather than the *last* `@` (`rfind('@')`). If a database password contains an `@` symbol (e.g. `postgres://root:p@ssw0rd@localhost/db`), `find('@')` splits `p` into `user_info`, while `@ssw0rd@localhost` is appended to `host_and_rest`, leaking the remainder of the password into logs.
- **Suggested fix:**
  1. Add `APIKEY`, `AUTHORIZATION`, `BEARER`, `SESSION`, and `SESSION_ID` to `is_secret_key`.
  2. Use `authority.rfind('@')` to locate the true boundary between userinfo and host authority per RFC 3986.

---

### m3 SAML Parser Comment Truncation and Tag Extraction Fragility

- **Where:** `rume/src/enterprise/saml.rs:76–89, 282–323`
- **What it says:**
  In `strip_comments`:
  ```rust
  while let Some(start) = rest.find("<!--") {
      out.push_str(&rest[..start]);
      if let Some(end) = rest[start..].find("-->") {
          rest = &rest[start + end + 3..];
      } else {
          return out;
      }
  }
  ```
  In attribute extraction:
  ```rust
  let attr_pattern = "Attribute Name=\"";
  ...
  while let Some(pos) = xml[search_idx..].find(attr_pattern) {
  ```
- **Why it's a problem:**
  1. If an XML assertion contains an unclosed comment `<!--` anywhere in the payload, `strip_comments` abruptly truncates the entire remainder of the XML string, causing parsing failures rather than returning a clean syntax error.
  2. Attribute extraction assumes exact string matching `Attribute Name="`. If an Identity Provider outputs single quotes (`Attribute Name='email'`), includes whitespace (`Attribute Name = "email"`), or orders attributes differently (`<Attribute FriendlyName="email" Name="email">`), the parser fails to extract user attributes.
- **Suggested fix:**
  Return a `SamlError::Malformed("unclosed XML comment")` error when `find("-->")` is `None`, and tokenize XML tag attributes rather than using hardcoded string patterns.

---

## Positive notes

- `#![forbid(unsafe_code)]` remains strictly enforced across the core crate (`rume/src/lib.rs:7`).
- Constant-time string and byte comparisons are uniformly implemented with `safe_equal` using `std::hint::black_box` to defeat compiler short-circuit optimizations.
- `ApiKeyScheme` was successfully hardened against heap memory exposure and timing leaks by migrating from plaintext linear scanning to SHA-256 hashed keys indexed in O(1) maps.
- `AuthError::ProviderError` correctly maps to `AppError::Internal` (HTTP 500), preserving RFC 9457 Problem Details semantics and distinguishing client credential errors from server infrastructure faults.
- Multi-scheme fallback in `AuthLayer` now functions as designed: stale session cookies gracefully return `Ok(None)`, allowing valid Bearer tokens or API keys to authenticate requests without early abortion.
- Declarative authorization policies (`RequireRole`, `RequirePermission`, `RequireAnyRole`, `RequireAllPermissions`) provide clean, type-safe separation between identity extraction and resource access control.

---

## Verdict

**Not ready for production.** The subsystem cannot be certified for production deployment until a security fix pass resolves the 3 Critical findings and 6 Major findings. Specifically: the SAML parser must abandon unanchored global substring matching in favor of XML tree parsing cryptographically bound to verified digital signature elements (C1, C2); `SqlAuditLogger` must parameterize query execution to eliminate SQL injection vulnerabilities on MySQL (C3); CSRF defenses must require signed HMAC tokens with `__Host-` prefixes and multi-cookie inspection (M1, M2); audit logging must redact sensitive metadata credentials (M3); SCIM entities must support `meta.version` for replay and concurrency protection (M4); and session handling must provide atomic session ID rotation and error propagation (M5, M6).