# Security, SAML & Protocol Hardening Review — v04

## Summary

This review conducts a whole-document audit of Rume's security, authentication, enterprise protocols, and hardening subsystems as of `HEAD`, evaluating `rume/src/security/` (`audit.rs`, `csrf.rs`, `headers.rs`), `rume/src/auth/` (`error.rs`, `identity.rs`, `layer.rs`, `policy.rs`, `schemes.rs`, `session.rs`, `redis.rs`), `rume/src/enterprise/` (`saml.rs`, `scim.rs`), `rume/src/env/` (`secrets.rs`, `loading.rs`, `interpolation.rs`), and `rume/src/helpers/` (`safe_equal.rs`). The remediation wave since v03 and commit `c550731` introduced critical architectural improvements: the SAML parser migrated to a namespace-aware `quick-xml` reader, eliminating Type-1 envelope injection and XML comment attacks while anchoring assertion claim extraction to the verified element slice; CSRF protection defaults to `cookie_secure: true` with multi-cookie inspection; `SessionScheme` propagates backend storage failures as HTTP 500 without silent guest degradation; session ID rotation saves before deleting; and audit logging redacts metadata credentials.

However, the subsystem is **not ready to ship without a fix pass**. While no Critical data-corruption vulnerabilities remain, **4 Major findings** and **5 Minor findings** undermine production security guarantees: (1) `CsrfConfig` continues to default to unauthenticated tokens (`secret: None`) without fail-closed startup validation or automatic key generation, permitting subdomain cookie-tossing; (2) `SessionScheme` aborts cookie evaluation on the first matching cookie name, enabling denial of service via subdomain cookie tossing; (3) `SamlParser` erroneously rejects standard SAML responses using enveloped signatures with empty reference URIs (`URI=""`), while failing open on assertions omitting `NotOnOrAfter`; and (4) `SqlAuditLogger` still relies on manual string formatting rather than driver-level parameterized statements, while hardcoding the PostgreSQL DDL across all database backends.

Executed verification (all passed, Docker daemon available):
- `cargo test -p rume --test integration -- enterprise::` → **11 passed; 0 failed**
- `cargo test -p rume --test integration -- security:: auth::` → **15 passed; 0 failed**
- `cargo test -p rume --all-features --test containers -- test_redis_session_store_save_load_ttl_and_destroy` → **1 passed; 0 failed**

---

## Prior decisions

Every prior finding in this lens's area across v01, v02, v03, and v03/diff/v01 is re-evaluated against the current codebase. Dispositions strictly follow the three canonical terms (**REAFFIRMED**, **CHALLENGED**, or **NOTE-NEW**).

### Prior Findings from v01 (`docs/reports/reviews/v01/security.md`)

- **v01 S-M1 (Path traversal in `file:` env loading)** — **REAFFIRMED**. Verified in `rume/src/env/loading.rs:252–267`. `resolve_value` canonicalizes both the base directory and the resolved target path, asserting `canonical_full.starts_with(&canonical_dir)` and rejecting escaping paths with `std::io::ErrorKind::PermissionDenied`.
- **v01 S-M2 (Secrets held as plain `String` in `ResolvedEnv`)** — **REAFFIRMED**. Verified in `rume/src/env/loading.rs:27–36, 50–60`. `ResolvedEnv` segregates sensitive values into `secrets: BTreeMap<String, SecretBox<String>>`, ensuring plaintext secrets are exposed only at final consumption via `expose_secret()`.
- **v01 S-M3 (CLI commands print secrets to terminal in plaintext)** — **REAFFIRMED**. Verified in `rume-cli/src/commands/env/check.rs:47–51` and `rume-cli/src/commands/config/check.rs:38–48`. Environment and configuration inspection commands redact secret keys and database URLs by default, requiring an explicit `--unredacted` flag for plaintext display.
- **v01 S-M4 (`AppError` renders raw `to_string()` into client JSON)** — **REAFFIRMED**. Verified in `rume/src/error.rs:173–275`. Client responses strictly follow RFC 9457 Problem Details (`application/problem+json`), suppressing internal database and system diagnostics from network responses.
- **v01 S-m1 (`redact()` heuristic false negatives)** — **REAFFIRMED**. Verified in `rume/src/env/secrets.rs:18–52`. Detection was broadened to match `_PASS`, `_PWD`, `_CRED`, `_KEY`, `APIKEY`, `AUTHORIZATION`, `BEARER`, `SESSION`, `SESSION_ID`, and `STRIPE_*`.
- **v01 S-m2 (`/health/ready` exposes pid and platform in `debug_info`)** — **REAFFIRMED**. Verified in `rume/src/routing/health.rs:289–294`. `debug_info()` returns an empty `BTreeMap::new()`.
- **v01 S-m3 (`Rejection` echoes parameter values and names in error response)** — **REAFFIRMED**. Verified in `rume/src/routing/rejection.rs:19–42`. Parameter rejections convert into generic Problem Details without reflecting user input.

### Prior Findings from v02 (`docs/reports/reviews/v02/security.md`)

- **v02 Security C1 (SAML assertion parser hardcodes expiration timestamps and lacks signature verification)** — **REAFFIRMED (Remediation verified)**. `NotBefore` and `NotOnOrAfter` are parsed via `parse_iso8601_strict` into Unix epoch seconds (`rume/src/enterprise/saml.rs:326–335`), and `SamlSignatureVerifier` enforces cryptographic XML-DSig verification (`saml.rs:467–470`).
- **v02 Security C2 (Multi-tenant cache key delimiter collision)** — **REAFFIRMED**. Audited and verified under Lens 4 (`rume/src/tenant/cache.rs`).
- **v02 Security M1 (Subdomain tenant resolver permits suffix-matching domain spoofing)** — **REAFFIRMED**. Audited and verified under Lens 4 (`rume/src/tenant/resolver.rs`).
- **v02 Security M2 (`HeaderTenantResolver` shares a single map for IDs and slugs)** — **REAFFIRMED**. Audited and verified under Lens 4 (`rume/src/tenant/resolver.rs`).
- **v02 Security M3 (`ApiKeyScheme` stores plaintext API keys in memory with early-exit linear scan)** — **REAFFIRMED**. Verified in `rume/src/auth/schemes.rs:86–114, 139–144`. API keys are hashed via SHA-256 upon registration into `HashMap<[u8; 32], AuthUser>` and looked up in O(1) time without linear scan timing side-channels.
- **v02 Security M4 (Double-submit CSRF layer lacks token issuance and is vulnerable to subdomain tossing)** — **CHALLENGED (Defective Remediation & Insecure Defaults)**. Token issuance and HMAC signing were added, but `CsrfConfig::default()` leaves `secret: None`, leaving naive double-submit active by default where any non-empty string is accepted as valid. Re-reported as finding M1.
- **v02 Security m1 (`AuthError::ProviderError` maps to HTTP 400 Bad Request instead of HTTP 500)** — **REAFFIRMED**. Verified in `rume/src/auth/error.rs:38`. `ProviderError` maps to `AppError::Internal` (HTTP 500).
- **v02 Security m2 (Stale session cookie in `SessionScheme` immediately aborts authentication chain)** — **REAFFIRMED**. Verified in `rume/src/auth/schemes.rs:202–204`. Stale or expired cookies return `Ok(None)`, allowing fallback to subsequent schemes in the chain.
- **v02 Security m3 (`SecurityHeadersLayer` injects HSTS header unconditionally over plain HTTP)** — **REAFFIRMED**. Verified in `rume/src/security/headers.rs:174–188, 201–207`. `hsts_require_tls: bool` defaults to `true` and suppresses HSTS emission over plain HTTP.

### Prior Findings from v03 (`docs/reports/reviews/v03/security-protocols.md`)

- **v03 Security C1 (SAML 2.0 Parser Vulnerable to XML Signature Wrapping (XSW) Attacks via Unbound Global Tag Extraction)** — **REAFFIRMED (Remediation verified)**. `quick_xml` namespace-aware reader isolates the assertion node (`assertion_start_offset..assertion_end_offset`) and validates claims strictly within the assertion subtree (`saml.rs:283–465`), defeating Type-1 envelope injection. Multiple assertion elements are rejected (`saml.rs:290–296`).
- **v03 Security C2 (Public `SamlParser::parse()` Bypasses Cryptographic Verification in Production)** — **CHALLENGED (Defective Remediation)**. `SamlParser::parse()` was annotated with `#[deprecated]`, but `parse()` and `parse_unverified()` remain public bypass paths, and `SamlError::MissingSignatureVerifier` is defined as dead code and never returned anywhere in the crate (`saml.rs:31–33, 233–241`). Re-reported as finding m4.
- **v03 Security C3 (SQL Injection Vulnerability in `SqlAuditLogger::record()` via String Formatting and MySQL Escapes)** — **CHALLENGED (Defective Remediation: Unparameterized Raw SQL Formatting Persists)**. While `escape_sql_literal` added dialect-specific backslash handling for MySQL and preserved `record.id`, `SqlAuditLogger::record()` still constructs raw SQL strings via `format!()` rather than using parameterized database statements (`audit.rs:214–222`). Re-reported as finding M4.
- **v03 Security M1 (Insecure Defaults in `CsrfConfig` Permit Subdomain Cookie-Tossing and Lack Mandatory HMAC Signing)** — **CHALLENGED (Unresolved Insecure Defaults)**. `cookie_secure` was updated to `true`, but `secret: None` remains the default, meaning `verify_token` accepts any non-empty string out of the box (`csrf.rs:64–77, 169–171`). Re-reported as finding M1.
- **v03 Security M2 (Subdomain Cookie-Tossing Causes Denial of Service via First-Cookie Selection Bias in `CsrfService`)** — **REAFFIRMED**. In `rume/src/security/csrf.rs:338–385`, `CsrfService::call` collects all matching cookies into `Vec<&str>` and checks `any_valid` and `has_valid_match` using `safe_equal`. Verified in `test_csrf_signed_tokens_and_cookie_tossing_protection`.
- **v03 Security M3 (Lack of Secret Redaction in Audit Logging Metadata)** — **REAFFIRMED**. Verified in `rume/src/security/audit.rs:188–194`. `record.metadata` is filtered through `redact_value` before JSON serialization.
- **v03 Security M4 (SCIM 2.0 Entities Lack `meta.version` for Optimistic Concurrency Control)** — **REAFFIRMED**. Verified in `rume/src/enterprise/scim.rs:36–49, 117`. `ScimMeta` and `ScimGroup` carry `version: Option<String>`.
- **v03 Security M5 (Missing Session ID Rotation Exposes Applications to Session Fixation Attacks)** — **REAFFIRMED**. Verified in `rume/src/auth/session.rs:86–104`. `SessionStore::regenerate_id` is defined and rotates session IDs.
- **v03 Security M6 (`SessionScheme` Silently Degrades Authenticated Users to Guests on Session Store Failures)** — **REAFFIRMED**. Verified in `rume/src/auth/schemes.rs:206–209`. Store errors return `Err(err)`, returning HTTP 500 rather than falling through to guest.
- **v03 m1 (Unvalidated Reverse Proxy Trust for `x-forwarded-proto` and Axum Relative URI Blindness)** — **REAFFIRMED (Remediation verified)**. `native_tls` and `trust_proxy_headers` were added to `SecurityHeadersConfig` (`headers.rs:41–44, 94–105`).
- **v03 m2 (Substring Heuristic Blind Spots and Leaky URL Parsing in `secrets.rs`)** — **REAFFIRMED**. `is_secret_key` covers `APIKEY`, `AUTHORIZATION`, `BEARER`, `SESSION`, `SESSION_ID`, and `redact_credential_urls_in_text` uses `authority.rfind('@')` (`secrets.rs:22–52, 97`).
- **v03 m3 (SAML Parser Comment Truncation and Tag Extraction Fragility)** — **REAFFIRMED**. `quick-xml` reader ignores comments without truncation, and tokenizes XML events natively (`saml.rs:257–426`).

### Prior Findings from v03/diff/v01 (`docs/reports/reviews/v03/diff/v01/security-protocols.md`)

- **v03/diff M1 (SAML 2.0 Signature Reference binding in `SamlParser` rejects standard signed Response envelopes and is fragile to namespace variants)** — **REAFFIRMED (Remediation verified)**. `signature_references` checks whether `ref_id` matches `assertion_id` OR `response_id` (`saml.rs:434–448`), and `test_saml_signature_wrapping_and_strict_validation` passes.
- **v03/diff M2 (Insecure defaults in `CsrfConfig` persist: missing mandatory HMAC secret and insecure cookies by default)** — **CHALLENGED (Defective Remediation)**. See v03 M1 above. `cookie_secure` was toggled to `true`, but `secret: None` persists. Re-reported as finding M1.
- **v03/diff m1 (Non-atomic session ID rotation in `SessionStore::regenerate_id` risks state loss on write failures)** — **REAFFIRMED**. `SessionStore::regenerate_id` calls `save(new_session)` first, destroying `old_id` only after successful write (`session.rs:99–102`).
- **v03/diff m2 (Audit logger SQL literal escaping causes data distortion in PostgreSQL and SQLite)** — **REAFFIRMED**. `escape_sql_literal` checks `is_mysql = self.backend.dialect() == "mysql"` before applying backslash/newline escaping (`audit.rs:158–174, 179–181`).

---

## Findings

### M1 Insecure Defaults in `CsrfConfig`: Unauthenticated Tokens Permitted by Default Without Fail-Closed Startup Validation

- **Where:** `rume/src/security/csrf.rs:64–77, 168–171, 213–220`
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
              cookie_secure: true,
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
  When developers instantiate CSRF protection using `CsrfLayer::new()`, `config.secret` is `None`. Under this configuration, `verify_token` accepts *any* non-empty string. If an attacker controls an insecure sibling subdomain or an unencrypted origin on the same domain, they can toss an arbitrary cookie `csrf_token=attacker_value` scoped to the parent domain. A cross-origin request carrying `x-csrf-token: attacker_value` satisfies `any_valid` (`!token.is_empty()`) and `has_valid_match` (`safe_equal`). The double-submit check passes completely, defeating CSRF protection out of the box. While helper method `with_random_secret()` was implemented in commit `1a3cf5f`, it is not called by `Default` or `CsrfLayer::new()`. Furthermore, `CsrfLayer::new()` emits no startup warning (`tracing::warn!` only exists in `with_config`).
- **Suggested fix:**
  1. Make `CsrfLayer::new()` auto-generate an ephemeral 32-byte secret (`CsrfConfig::default().with_random_secret()`) or require an HMAC secret at startup.
  2. Emit a `tracing::warn!` in `CsrfLayer::new()` if `secret` is `None`.
  3. When `secret` is `None`, log an explicit warning or provide an explicit builder `CsrfConfig::insecure_without_secret_for_testing()` so that running without token authentication is deliberate rather than accidental.
- **Evidence:**
  - *Static:* `rume/src/security/csrf.rs:64–77, 168–171, 213–220`.
  - *Executed:* `cargo test -p rume --test integration -- security::test_csrf_protection_lifecycle` passes, confirming that requests with unauthenticated matching arbitrary strings (`secret-token-xyz`) are accepted when `secret` is `None`.

---

### M2 Subdomain Cookie-Tossing Denial of Service via First-Cookie Selection Bias in `SessionScheme`

- **Where:** `rume/src/auth/schemes.rs:189–213`
- **What it says:**
  ```rust
  // Parse cookies: name=val; name2=val2
  for cookie in cookie_str.split(';') {
      if let Some((name, val)) = cookie.trim().split_once('=')
          && name == self.cookie_name
      {
          match self.store.load(val).await {
              Ok(Some(session)) => {
                  let mut user = AuthUser::new(session.user_id)
                      .with_roles(session.roles)
                      .with_permissions(session.permissions);
                  user.metadata = session.data;
                  return Ok(Some(user));
              }
              Ok(None) => {
                  // Stale or expired cookie: allow fallback to subsequent schemes in the chain
                  return Ok(None);
              }
              Err(err) => {
                  // Infrastructure or provider error: do not silently degrade to guest!
                  return Err(err);
              }
          }
      }
  }
  ```
- **Why it's a problem:**
  In HTTP, if cookies with the same name are set on both a parent domain (`.example.com`) and a host (`app.example.com`), the browser sends both in the `Cookie` header (e.g. `Cookie: rume_session=tossed_invalid; rume_session=valid_host_session`). In `SessionScheme::authenticate`, the loop encounters the first cookie matching `self.cookie_name`. When `self.store.load("tossed_invalid").await` returns `Ok(None)`, line 204 immediately executes `return Ok(None);`. It does **not** continue checking subsequent cookies with that name. Consequently, an attacker on any sibling subdomain can inject a bogus parent-domain session cookie, permanently locking the user out of their authenticated session across the application (first-cookie selection denial of service).
- **Suggested fix:**
  Replace `return Ok(None);` with `continue;` inside the loop. The scheme should continue inspecting all cookies matching `self.cookie_name`, returning `Ok(Some(user))` if *any* matching cookie contains a valid active session. Return `Ok(None)` only after all matching cookies have been exhausted without finding a valid session.
- **Evidence:**
  - *Static:* `rume/src/auth/schemes.rs:189–213`.
  - *Executed:* Verified via `test_stale_session_cookie_falls_back_to_bearer_token` in `tests/integration/auth.rs`.

---

### M3 SAML 2.0 Parser Rejects Enveloped Document Signatures (`URI=""`) and Fails Open on Missing Expiration (`NotOnOrAfter`)

- **Where:** `rume/src/enterprise/saml.rs:316–324, 434–448, 498–505`
- **What it says:**
  ```rust
  // In Reference extraction:
  if local_k.as_ref() == "URI" {
      let uri = attr.value.as_ref();
      let ref_id = uri.strip_prefix('#').unwrap_or(uri);
      signature_references.push(ref_id.to_string());
  }
  ...
  // In reference validation:
  for ref_id in &signature_references {
      let matches_assertion = assertion_id.as_deref() == Some(ref_id.as_str());
      let matches_response = response_id.as_deref() == Some(ref_id.as_str());
      if !matches_assertion && !matches_response {
          return Err(SamlError::SignatureWrappingDetected(format!(
              "signature reference URI '#{ref_id}' does not match {expected}"
          )));
      }
  }
  ...
  // In SamlParser::validate:
  if let Some(not_on_or_after) = assertion.not_on_or_after
      && current_epoch_sec >= not_on_or_after
  {
      return Err(SamlError::Expired { ... });
  }
  ```
- **Why it's a problem:**
  1. **Rejection of standard enveloped document signatures:** Under XML-DSig and SAML 2.0 Core Section 5, standard Identity Providers (Okta, Azure AD, ADFS) frequently sign the entire SAML document using an enveloped signature where the signature reference has an empty URI: `<ds:Reference URI="">`. In `SamlParser`, `ref_id` is extracted as `""`. Lines 435–447 assert that `ref_id` must match `assertion_id` or `response_id`. Because neither ID is empty, the parser rejects standard signed responses with `SignatureWrappingDetected("signature reference URI '' does not match ...")`.
  2. **Indefinite replay vulnerability (Missing `NotOnOrAfter`):** If an Identity Provider assertion or a tampered assertion omits the `<saml2:Conditions>` element or omits `NotOnOrAfter`, `assertion.not_on_or_after` evaluates to `None`. In `SamlParser::validate` (and `validate_strict`), expiration checks only execute inside `if let Some(not_on_or_after) = assertion.not_on_or_after`. When missing, the check is skipped and the assertion is accepted as valid indefinitely, violating OASIS SAML 2.0 Web Browser SSO replay prevention rules.
  3. **Zero clock skew tolerance:** Expiration comparison `current_epoch_sec >= not_on_or_after` and `current_epoch_sec < not_before` provide zero clock skew tolerance, causing transient authentication failures under ordinary sub-second NTP drift between IdP and SP servers.
- **Suggested fix:**
  1. In `signature_references` validation, treat `ref_id.is_empty()` (empty URI `URI=""`) as a valid reference when the root element is `<Response>`.
  2. In `SamlParser::validate` (and `validate_strict`), require `not_on_or_after` to be present by default (`assertion.not_on_or_after.ok_or_else(|| SamlError::Malformed("missing mandatory NotOnOrAfter condition".into()))?`).
  3. Support a configurable `clock_skew_secs` (e.g. 60 seconds tolerance) during timestamp validation.
- **Evidence:**
  - *Static:* `rume/src/enterprise/saml.rs:316–324, 434–448, 498–505`.
  - *Executed:* `cargo test -p rume --test integration -- enterprise::test_saml_assertion_parsing_and_validation` passes without `NotOnOrAfter` present on line 39 of the test assertion fixture, verifying that assertions lacking expiration timestamps pass validation without errors.

---

### M4 `SqlAuditLogger` Remains Unparameterized, Relying on Manual String Formatting and Hardcoding PostgreSQL Schema

- **Where:** `rume/src/security/audit.rs:124–139, 214–223`
- **What it says:**
  ```rust
  pub const SCHEMA: &'static str = include_str!("../persistence/sql/postgres/audit_schema.sql");

  pub async fn init_table(&self) -> Result<(), AuditError> {
      self.backend
          .execute(Self::SCHEMA)
          .await
          .map_err(|e| AuditError::Sink(e.to_string()))?;
      Ok(())
  }
  ...
  let stmt = format!(
      "INSERT INTO rume_audit_log (id, action, resource, status, actor_id, target_id, ip_address, metadata, created_at) VALUES ({id}, {action}, {resource}, '{status_str}', {actor_val}, {target_val}, {ip_val}, {meta_val}, {})",
      record.timestamp_ms
  );

  self.backend
      .execute(&stmt)
      .await
      .map_err(|e| AuditError::Sink(e.to_string()))?;
  ```
- **Why it's a problem:**
  1. **Unparameterized SQL:** While `escape_sql_literal` mitigates basic quotes and MySQL backslash escapes, `SqlAuditLogger` continues to construct SQL commands using string formatting rather than bound query parameters (`sqlx::query` with `$1..$N` or `?`). This leaves audit logging reliant on manual character escape heuristics rather than driver-level AST isolation.
  2. **Schema Hardcoding:** `SqlAuditLogger::init_table` executes `Self::SCHEMA` unconditionally across all backends. Dialect-specific schema files exist at `rume/src/persistence/sql/mysql/audit_schema.sql` and `rume/src/persistence/sql/sqlite/audit_schema.sql`, but are completely ignored. On MySQL, executing PostgreSQL's DDL omits `ENGINE=InnoDB` and uses `BIGINT NOT NULL` rather than `BIGINT UNSIGNED NOT NULL`. On SQLite, it executes `VARCHAR`/`BIGINT` instead of standard `TEXT`/`INTEGER`.
- **Suggested fix:**
  1. Select the schema string dynamically in `init_table` based on `self.backend.dialect()` (`postgres`, `mysql`, `sqlite`).
  2. Parameterize statement execution on `DatabaseBackend` to pass bound parameters, or use driver-level query binding for audit insertions.
- **Evidence:**
  - *Static:* `rume/src/security/audit.rs:124–139, 214–223`.
  - *Executed:* `cargo test -p rume --test integration -- security::test_audit_logging` passes against SQLite backend using PostgreSQL DDL.

---

### m1 Insecure `Forwarded` Header Substring Matching and Unchecked Reverse Proxy Trust in `SecurityHeadersService`

- **Where:** `rume/src/security/headers.rs:43–44, 60, 176–188`
- **What it says:**
  ```rust
  let is_https = config.native_tls
      || req.uri().scheme() == Some(&axum::http::uri::Scheme::HTTPS)
      || (config.trust_proxy_headers
          && (req
              .headers()
              .get("x-forwarded-proto")
              .and_then(|v| v.to_str().ok())
              .map(|v| v.eq_ignore_ascii_case("https"))
              .unwrap_or(false)
              || req
                  .headers()
                  .get("forwarded")
                  .and_then(|v| v.to_str().ok())
                  .map(|v| v.to_ascii_lowercase().contains("proto=https"))
                  .unwrap_or(false)));
  ```
- **Why it's a problem:**
  1. `trust_proxy_headers` defaults to `true`. Without verifying the network peer IP against a configured list of trusted reverse proxies, any direct HTTP client can send `x-forwarded-proto: https` to trick Rume into emitting `Strict-Transport-Security` over plain HTTP in violation of RFC 6797 §7.2.
  2. The check for RFC 7239 `Forwarded` uses `.contains("proto=https")`. An attacker can send `Forwarded: for=192.0.2.60;host="proto=https.evil.com"` where `"proto=https"` appears inside an unrelated parameter, falsely causing `is_https` to evaluate to `true`.
- **Suggested fix:**
  Parse RFC 7239 `Forwarded` parameters by splitting on `;` and matching the `proto` parameter specifically (`pair.trim().split_once('=').map(|(k, v)| k == "proto" && v.eq_ignore_ascii_case("https"))`). Provide a trusted proxy IP/subnet configuration before honoring upstream proxy headers.
- **Evidence:**
  - *Static:* `rume/src/security/headers.rs:43–44, 176–188`.

---

### m2 `CsrfService` Reads Only First `Cookie` Header, Ignoring Multi-Header and RFC 7540 Split Cookie Headers

- **Where:** `rume/src/security/csrf.rs:281–295, 338–355`
- **What it says:**
  ```rust
  let matching_cookies: Vec<&str> = req
      .headers()
      .get(COOKIE)
      .and_then(|val| val.to_str().ok())
      .map(|cookies| {
          cookies
              .split(';')
              .filter_map(|pair| {
  ```
- **Why it's a problem:**
  `req.headers().get(COOKIE)` retrieves only the first `Cookie` header. Under HTTP/2 (RFC 7540 Section 8.1.2.5) or HTTP/1.1 reverse proxies that split cookies across multiple header lines, cookies in subsequent `Cookie` headers are ignored. A legitimate CSRF cookie located in a secondary `Cookie` header field will not be inspected, causing false-positive `403 CSRF_TOKEN_MISSING` rejections.
- **Suggested fix:**
  Iterate across all cookie headers using `req.headers().get_all(COOKIE)`:
  ```rust
  let matching_cookies: Vec<&str> = req
      .headers()
      .get_all(COOKIE)
      .iter()
      .filter_map(|val| val.to_str().ok())
      .flat_map(|cookies| cookies.split(';'))
      .filter_map(|pair| { ... })
      .collect();
  ```
- **Evidence:**
  - *Static:* `rume/src/security/csrf.rs:281–295, 338–355`.

---

### m3 Non-Atomic Session ID Rotation in `RedisSessionStore`

- **Where:** `rume/src/auth/session.rs:86–104`, `rume/src/auth/redis.rs:54–131`
- **What it says:**
  `RedisSessionStore` does not implement `regenerate_id`, falling back to the trait default:
  ```rust
  let existing = self.load(old_id).await?...;
  let new_session = Session { id: uuid::Uuid::new_v4().to_string(), ... };
  self.save(new_session.clone()).await?;
  if let Err(err) = self.destroy(old_id).await { ... }
  ```
- **Why it's a problem:**
  Session rotation requires three sequential network round-trips to Redis (`GET`, `SET EX`, `DEL`). If `destroy` fails or the node crashes mid-operation, both the old and new session IDs remain active in Redis until TTL expiration, leaving a window where a compromised pre-rotation session ID can still authenticate requests.
- **Suggested fix:**
  Override `regenerate_id` in `RedisSessionStore` using an atomic Redis Lua script or pipeline that reads the existing session TTL, copies the data to the new key, and deletes the old key in a single transaction.
- **Evidence:**
  - *Static:* `rume/src/auth/session.rs:86–104`, `rume/src/auth/redis.rs:54–131`.
  - *Executed:* `containers::test_redis_session_store_save_load_ttl_and_destroy` passes sequential rotation.

---

### m4 Dead Error Variant `SamlError::MissingSignatureVerifier` and Unchecked Optional Verifiers

- **Where:** `rume/src/enterprise/saml.rs:31–33, 233–241, 253–256`
- **What it says:**
  ```rust
  /// Missing mandatory digital signature verifier.
  #[error("signature verification required: missing signature verifier")]
  MissingSignatureVerifier,
  ...
  pub fn parse_with_verifier(
      xml: &str,
      verifier: Option<&dyn SamlSignatureVerifier>,
  ) -> Result<SamlAssertion, SamlError> {
  ```
- **Why it's a problem:**
  `SamlError::MissingSignatureVerifier` is defined but never returned anywhere in `rume`. Because `parse_with_verifier` accepts `Option<&dyn SamlSignatureVerifier>` and executes without error when `verifier` is `None`, unverified assertions can still be parsed in production via `SamlParser::parse_unverified(xml)`.
- **Suggested fix:**
  Enforce signature verification by removing `Option` from `parse_with_verifier(xml, verifier: &dyn SamlSignatureVerifier)`. Restrict unverified parsing to explicitly named testing helpers (`parse_unverified_insecure_for_testing`) and return `MissingSignatureVerifier` when signature verification cannot be performed.
- **Evidence:**
  - *Static:* `grep -rn MissingSignatureVerifier rume/src/` returns only the enum variant definition in `saml.rs:33`.

---

### m5 SCIM 2.0 Casing and Attribute Type Deviations from RFC 7643 and RFC 7644

- **Where:** `rume/src/enterprise/scim.rs:64, 139–159`
- **What it says:**
  ```rust
  #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
  #[serde(rename_all = "camelCase")]
  pub struct ScimListResponse<T> {
      ...
      pub resources: Vec<T>,
  }
  ...
  pub struct ScimUser {
      ...
      pub roles: Vec<String>,
  }
  ```
- **Why it's a problem:**
  1. Under `#[serde(rename_all = "camelCase")]`, `resources` serializes to `"resources"` (lowercase 'r'). However, RFC 7644 Section 3.4.2 explicitly specifies `"Resources"` (uppercase 'R') for list responses. Strict SCIM clients (such as Azure AD or Okta SCIM compliance checkers) reject `"resources"`.
  2. In `ScimUser`, `roles` is typed as `Vec<String>`, which serializes to `["admin"]`. RFC 7643 Section 4.1.2 defines `roles` as a multi-valued complex attribute containing sub-attributes (`value`, `display`, `type`, `primary`).
- **Suggested fix:**
  1. Add `#[serde(rename = "Resources")]` to `ScimListResponse.resources`.
  2. Define `ScimRole { pub value: String, pub primary: bool, pub role_type: Option<String> }` or support both string and object deserialization for `ScimUser.roles`.
- **Evidence:**
  - *Static:* `rume/src/enterprise/scim.rs:64, 139–159`.
  - *Executed:* `cargo test -p rume --test integration -- enterprise::test_scim_models_and_json_serialization` passes.

---

## Positive notes

- `#![forbid(unsafe_code)]` remains strictly enforced on the core crate (`rume/src/lib.rs:7`).
- Constant-time comparison `safe_equal` utilizes `std::hint::black_box` to prevent compiler vectorization and short-circuit optimizations (`rume/src/helpers/safe_equal.rs:41`).
- `ApiKeyScheme` prevents plaintext secret storage and linear scan timing leaks by hashing keys with SHA-256 and storing them in an indexed map (`rume/src/auth/schemes.rs:109–113, 139–144`).
- SAML claim extraction is now strictly anchored to the assertion subtree using `quick_xml` namespace resolution, completely blocking Type-1 envelope injection and XML comment attacks (`rume/src/enterprise/saml.rs:257–426`).
- `SessionScheme` no longer degrades authenticated sessions to guests on infrastructure timeouts, surfacing provider errors as HTTP 500 (`rume/src/auth/schemes.rs:206–209`).
- Session ID rotation in `SessionStore::regenerate_id` commits the fresh session before destroying the old ID, preventing session loss on save failures (`rume/src/auth/session.rs:99–102`).
- Audit logging automatically redacts sensitive credentials and URLs from `record.metadata` before JSON serialization (`rume/src/security/audit.rs:188–194`).
- Environment variable `file:` resolution canonicalizes paths and rejects traversal attempts outside the root directory with `std::io::ErrorKind::PermissionDenied` (`rume/src/env/loading.rs:252–267`).
- Declarative authorization policies (`RequireRole`, `RequirePermission`, `RequireAnyRole`, `RequireAllPermissions`) provide clean, type-safe authorization boundaries separate from identity extraction (`rume/src/auth/policy.rs:26–65`).

---

## Verdict

**Not ready to ship without a fix pass.** (Whole-document mode).

The security, SAML, and protocol hardening subsystems have eliminated all prior Critical findings, but cannot be approved for production deployment until a fix pass resolves the **4 Major findings**:
1. **Finding M1**: `CsrfConfig` continues to default to unauthenticated double-submit tokens (`secret: None`), allowing subdomain cookie-tossing attacks unless developers manually inject an HMAC key.
2. **Finding M2**: `SessionScheme` suffers from first-cookie selection bias, aborting authentication on the first matching cookie name and enabling denial of service via tossed parent-domain cookies.
3. **Finding M3**: `SamlParser` rejects standard enveloped document signatures (`URI=""`) and fails open by accepting assertions that omit `NotOnOrAfter` expiration conditions.
4. **Finding M4**: `SqlAuditLogger` relies on unparameterized manual string formatting and hardcodes the PostgreSQL schema file across MySQL and SQLite backends.

Once a fix pass addressing M1–M4 lands, the subsystem will meet production security standards.
