# Security Review — v02

## Summary

Rume v02 successfully resolves all security findings from v01: path traversal in `file:` environment resolution is sandboxed, secrets are isolated in `secrecy::SecretBox`, inspection commands default to redaction, and `AppError` adopts RFC 9457 Problem Details without leaking internal diagnostics. However, the new enterprise, multi-tenant, and authentication surface introduces critical vulnerabilities: SAML 2.0 assertion parsing hardcodes expiration timestamps to `None` and lacks XML signature verification, multi-tenant cache keys suffer from delimiter collisions allowing cross-tenant data leakage, the subdomain tenant resolver allows host suffix spoofing, and `HeaderTenantResolver` conflates ID and slug namespaces. These vulnerabilities must be resolved before enterprise, multi-tenant, or authentication features can be safely deployed.

## Prior decisions

- **S-M1 (Path traversal in `file:` env loading)** — **REAFFIRMED**. Verified in `crates/core/src/env/loading.rs:254–267`. `resolve_value` now canonicalizes both `dir` and target path `full`, rejecting escaping paths via `!canonical_full.starts_with(&canonical_dir)` with `PermissionDenied`. Integration tests `file_value_traversal_is_rejected` and `file_value_absolute_path_is_rejected` confirm the fix.
- **S-M2 (Secrets held as plain `String` in `ResolvedEnv`)** — **REAFFIRMED**. Verified in `crates/core/src/env/loading.rs:27–36, 174–188`. `ResolvedEnv` now stores secret variables in `secrets: BTreeMap<String, SecretBox<String>>` separate from plain `values`, wrapping values into `SecretBox` at load time and only exposing plaintext via `expose_secret()` on demand.
- **S-M3 (CLI commands print secrets to terminal in plaintext)** — **REAFFIRMED**. Verified in `crates/cli/src/commands/env/check.rs:47–51, 74–78` and `crates/cli/src/commands/config/check.rs:38–48, 77–83`. Both `rume env check` and `rume config check` now redact sensitive keys and credential URLs by default, requiring an explicit `--unredacted` flag for plaintext output.
- **S-M4 (`AppError` renders raw `to_string()` into client JSON)** — **REAFFIRMED**. Verified in `crates/core/src/error.rs:173–275`. Responses now strictly follow RFC 9457 Problem Details (`application/problem+json`). `BadRequest` and `Internal` log sensitive diagnostics server-side with `warn!` and `error!` while returning static generic detail strings to HTTP clients.
- **S-m1 (`redact()` heuristic false negatives)** — **REAFFIRMED**. Verified in `crates/core/src/env/secrets.rs:18–42`. `is_secret_key` was broadened to match `_PASS`, `_PWD`, `_CRED`, `_KEY`, `STRIPE_*`, `PRIVATE_KEY`, and `redact_diagnostic_text` scrubs credential URLs and known secrets from diagnostics.
- **S-m2 (`/health/ready` exposes pid and platform in `debug_info`)** — **REAFFIRMED**. Verified in `crates/core/src/routing/health.rs:289–294`. `debug_info()` now returns an empty `BTreeMap::new()`, preventing reconnaissance leakage.
- **S-m3 (`Rejection` echoes parameter values and names in error response)** — **REAFFIRMED**. Verified in `crates/core/src/routing/rejection.rs:19–42`. `Rejection::into_response` converts into `AppError::BadRequest`, returning generic Problem Details to the client while keeping extraction messages in server logs.
- **Enterprise SAML 2.0 assertions (`crates/core/src/enterprise/saml.rs`)** — **NOTE-NEW**. New subsystem introduced in v02; evaluated in C1.
- **Enterprise SCIM 2.0 schemas (`crates/core/src/enterprise/scim.rs`)** — **NOTE-NEW**. New subsystem introduced in v02; reviewed for schema compliance.
- **Multi-tenant boundaries (`crates/core/src/tenant/`)** — **NOTE-NEW**. New subsystem introduced in v02; evaluated in C2, M1, and M2.
- **Authentication schemes & session storage (`crates/core/src/auth/`)** — **NOTE-NEW**. New subsystem introduced in v02; evaluated in M3, m1, and m2.
- **Security middleware (`crates/core/src/security/`)** — **NOTE-NEW**. New subsystem introduced in v02; evaluated in M4 and m3.

## Findings

### C1 SAML 2.0 assertion parser hardcodes expiration timestamps to `None` and lacks XML signature verification

**Where:** `crates/core/src/enterprise/saml.rs` lines 49–66 (`extract_tag`), lines 147–156 (`SamlParser::parse`), and lines 159–201 (`SamlParser::validate`).

**What it says:**

```rust
        Ok(SamlAssertion {
            id,
            issuer,
            subject,
            audience,
            not_before: None,
            not_on_or_after: None,
            attributes,
        })
```

`SamlParser::parse` constructs `SamlAssertion` with `not_before: None` and `not_on_or_after: None` unconditionally, never parsing `NotBefore` or `NotOnOrAfter` from `<saml2:Conditions>`. Furthermore, `SamlParser` uses ad-hoc substring searching (`xml.find`) to locate tags without parsing XML structure, stripping comments, or verifying XML digital signatures (XML-DSig).

**Why it's a problem:**
1. Complete bypass of time-validity validation: In `SamlParser::validate`, expiration checks only run if `assertion.not_on_or_after` is `Some(...)`. Because `not_on_or_after` is always `None`, timestamp validation is a no-op. Any captured SAML assertion can be replayed indefinitely.
2. Lack of digital signature verification: SAML 2.0 relies on cryptographic XML-DSig to authenticate assertions. Without signature verification against the Identity Provider's certificate, an attacker can trivially forge an XML assertion specifying any arbitrary `NameID` (e.g. `admin@victim.com`) and authenticate as that user.
3. XML comment injection: Because `extract_tag` searches for `<tag>` anywhere in the raw string, an attacker can embed fake identity elements inside XML comments (e.g. `<!-- <saml2:NameID>admin@corp.com</saml2:NameID> --> <saml2:NameID>attacker@evil.com</saml2:NameID>`), causing the parser to authenticate the victim's identity.

**Suggested fix:**
1. Parse `NotBefore` and `NotOnOrAfter` XML attributes from the `<saml2:Conditions>` element into epoch timestamps.
2. Require XML-DSig cryptographic signature verification against a configured IdP certificate/public key before accepting an assertion.
3. Replace ad-hoc string searching with a robust, hardened XML parser that properly handles namespaces, ignores comments/CDATA, and enforces XML schema bounds.

### C2 Multi-tenant cache key delimiter collision permits cross-tenant cache poisoning and data leakage

**Where:** `crates/core/src/tenant/cache.rs` lines 23–29 (`scoped_key` and `tenant_tag`).

**What it says:**

```rust
    fn scoped_key(tenant_id: &str, key: &str) -> String {
        format!("tenant:{tenant_id}:{key}")
    }

    fn tenant_tag(tenant_id: &str) -> String {
        format!("tenant:{tenant_id}")
    }
```

Tenant cache keys are constructed by concatenating `tenant_id` and `key` with a colon delimiter, with no validation or escaping on `tenant_id`.

**Why it's a problem:** Colon (`:`) is an unescaped delimiter. If Tenant A has ID `t1` and sets key `user:42`, the cache key is `tenant:t1:user:42`. If Tenant B has ID `t1:user` and accesses key `42`, its cache key is also `tenant:t1:user:42`. Tenant B can read Tenant A's cached data (cross-tenant information leakage) or overwrite it with malicious data (cross-tenant cache poisoning). Similarly, `tenant_tag` allows tag collision during bulk invalidation (`invalidate_tenant`).

**Suggested fix:** Strictly validate that `tenant_id` contains only alphanumeric characters and hyphens (`^[a-zA-Z0-9_-]+$`, forbidding colons), or length-prefix the tenant segment (e.g. `format!("tenant:{}:{}:{key}", tenant_id.len(), tenant_id)`) to guarantee unambiguous key boundaries.

### M1 Subdomain tenant resolver permits suffix-matching domain spoofing

**Where:** `crates/core/src/tenant/resolver.rs` lines 133–149.

**What it says:**

```rust
            // Remove optional port: "acme.myplatform.com:8080" -> "acme.myplatform.com"
            let host_no_port = host_str.split(':').next().unwrap_or(host_str);

            if let Some(subdomain) = host_no_port.strip_suffix(&self.base_domain) {
                let subdomain = subdomain.trim_end_matches('.');
                if !subdomain.is_empty() {
                    let map = self
                        .tenants
                        .read()
                        .map_err(|e| TenantError::ResolverError(e.to_string()))?;

                    if let Some(tenant) = map.get(subdomain) {
                        return Ok(Some(tenant.clone()));
                    } else {
                        return Err(TenantError::TenantNotFound(subdomain.to_string()));
                    }
                }
            }
```

When `base_domain` is provided without a leading dot (e.g. `"app.platform.io"` as configured in `crates/core/tests/tenant_suite.rs:78`), `host_no_port.strip_suffix(&self.base_domain)` matches any host that ends with that string.

**Why it's a problem:** An attacker pointing a rogue domain such as `evilapp.platform.io` or `fakeapp.platform.io` to the server will have `app.platform.io` stripped, leaving `evil` or `fake`. If a tenant exists with that slug, the request is accepted and routed to that tenant. Furthermore, multi-level subdomains (such as `attacker.acme.app.platform.io`) produce `attacker.acme`, which does not validate single-label subdomain boundaries.

**Suggested fix:** Normalize `base_domain` to ensure it has a leading dot (`format!(".{}", self.base_domain.trim_start_matches('.'))`), verify that the extracted subdomain does not contain internal dots (`!subdomain.contains('.')`), and validate that the subdomain adheres to strict slug syntax.

### M2 `HeaderTenantResolver` shares a single hash map for both ID and slug, causing tenant hijacking via collision

**Where:** `crates/core/src/tenant/resolver.rs` lines 49–56 (`add_tenant`).

**What it says:**

```rust
    pub fn add_tenant(self, tenant: Tenant) -> Self {
        if let Ok(mut map) = self.tenants.write() {
            map.insert(tenant.id.clone(), tenant.clone());
            map.insert(tenant.slug.clone(), tenant);
        }
        self
    }
```

Both `tenant.id` and `tenant.slug` are inserted into the same flat `BTreeMap<String, Tenant>`.

**Why it's a problem:** If Tenant 1 has slug `acme` (with ID `t-001`), and Tenant 2 has ID `acme` (with slug `globex`), inserting Tenant 2 will overwrite Tenant 1's slug entry in the map. Incoming requests with `X-Tenant-ID: acme` will be resolved as Tenant 2 instead of Tenant 1. Conflating the identifier and slug namespaces without collision checking or namespace prefixing allows one tenant registration to silently hijack another tenant's traffic.

**Suggested fix:** Maintain separate lookup maps (`by_id` and `by_slug`), or namespace keys within the map (`format!("id:{id}")` and `format!("slug:{slug}")`), or reject registration if an ID collides with an existing slug or vice versa.

### M3 `ApiKeyScheme` stores plaintext API keys in memory with early-exit linear scan

**Where:** `crates/core/src/auth/schemes.rs` lines 86 (`keys: Arc<BTreeMap<String, AuthUser>>`) and lines 133–140.

**What it says:**

```rust
            for (known_key, user) in self.keys.iter() {
                if safe_equal(key_str, known_key) {
                    return Ok(Some(user.clone()));
                }
            }

            Err(AuthError::InvalidCredentials("unrecognized api key".into()))
```

API keys are stored in plaintext in memory. Authenticating a request performs an `O(N)` linear iteration over the BTreeMap, returning immediately upon the first match.

**Why it's a problem:**
1. Secrets in memory: API keys sit unhashed in heap memory for the process lifetime, exposing them to memory dumps and core dumps.
2. Timing leakage across keys: Although `safe_equal` prevents byte-by-byte timing leaks when comparing two equal-length strings, the loop aborts as soon as a key matches. The elapsed response time reveals where in the key set a match occurred (or if none matched), leaking information about valid API keys.
3. Performance degradation: For systems with numerous API keys, linear scanning on every request imposes unnecessary CPU overhead.

**Suggested fix:** Store cryptographic hashes (e.g. SHA-256) of API keys in a map. When validating, hash the incoming key with SHA-256 and perform an `O(1)` constant-time lookup.

### M4 Double-submit CSRF layer lacks token issuance and is vulnerable to subdomain cookie tossing

**Where:** `crates/core/src/security/csrf.rs` lines 122–195 (`CsrfService::call`).

**What it says:**

`CsrfService` only verifies mutating requests by checking whether `safe_equal(header_token, cookie_token)` succeeds. It does not provide any handler, middleware, or hook to generate and issue CSRF tokens or set the CSRF cookie on safe requests.

**Why it's a problem:**
1. Lack of token issuance: Developers cannot use `CsrfLayer` without manually implementing token generation and cookie management. Rume provides no helper to generate secure random tokens or emit `Set-Cookie` with appropriate flags (`SameSite`, `Path`, `Secure`).
2. Cookie-tossing vulnerability: In naive double-submit cookie validation without HMAC signing or server-side binding, an attacker who controls a sibling subdomain (or an attacker on the same network over unencrypted HTTP) can set a cookie `csrf_token=evil_token` scoped to the parent domain. When the victim visits the attacker's site, a cross-origin request carrying `x-csrf-token: evil_token` matches the injected cookie, bypassing CSRF protection.

**Suggested fix:** Provide automated CSRF token generation and cookie injection on `GET`/`HEAD` requests with hardened defaults (`SameSite=Lax`, `Secure`, `HttpOnly=false`, `Path=/`). Adopt HMAC-signing for double-submit tokens (signed with an application key) or use the `__Host-` cookie prefix to prevent subdomain cookie tossing.

### m1 `AuthError::ProviderError` maps to HTTP 400 Bad Request instead of HTTP 500 Internal Server Error

**Where:** `crates/core/src/auth/error.rs` lines 38–41.

**What it says:**

```rust
            AuthError::ProviderError(msg) => AppError::BadRequest {
                code: "AUTH_ERROR",
                message: msg,
            },
```

`AuthError::ProviderError` is converted to `AppError::BadRequest` (HTTP 400).

**Why it's a problem:** A provider error indicates an internal system failure (database unreachable, cache failure, lock acquisition failure, or upstream authentication provider error). Returning 400 Bad Request misclassifies a 5xx server fault as a 4xx client error, corrupting API client retry logic, error budgets, and operational metrics.

**Suggested fix:** Map `AuthError::ProviderError(msg)` to `AppError::Internal { source: InternalSource(msg), request_id: None }`.

### m2 Stale session cookie in `SessionScheme` immediately aborts authentication chain

**Where:** `crates/core/src/auth/schemes.rs` lines 188–198.

**What it says:**

```rust
                    let session_opt = self.store.load(val).await?;
                    if let Some(session) = session_opt {
                        let mut user = AuthUser::new(session.user_id)
                            .with_roles(session.roles)
                            .with_permissions(session.permissions);
                        user.metadata = session.data;
                        return Ok(Some(user));
                    } else {
                        return Err(AuthError::Expired);
                    }
```

When an expired or unknown session cookie is received, `SessionScheme` returns `Err(AuthError::Expired)`.

**Why it's a problem:** In `AuthService::call`, any scheme returning an `Err` immediately aborts the authentication pipeline and returns 401 Unauthorized to the client without trying remaining schemes. If an application configures multiple authentication schemes (e.g. `SessionScheme` followed by `BearerTokenScheme` for hybrid web/API usage) or uses `AuthLayer::optional()`, a stale browser session cookie permanently breaks valid Bearer token requests and guest access.

**Suggested fix:** When a session cookie is expired or not found, return `Ok(None)` so that fallback authentication schemes (or optional guest access) can proceed.

### m3 `SecurityHeadersLayer` injects HSTS header unconditionally over plain HTTP

**Where:** `crates/core/src/security/headers.rs` lines 158–163.

**What it says:**

```rust
            if let Some(hsts) = &config.strict_transport_security
                && !headers.contains_key(STRICT_TRANSPORT_SECURITY)
                && let Ok(val) = HeaderValue::from_str(hsts)
            {
                headers.insert(STRICT_TRANSPORT_SECURITY, val);
            }
```

`Strict-Transport-Security` is injected into every response whenever configured, regardless of whether the request was served over HTTPS or plain HTTP.

**Why it's a problem:** Under RFC 6797 Section 7.2, an HTTP host MUST NOT include the STS header in HTTP responses over non-secure transport. In local development or environments where TLS is terminated at a reverse proxy that strips HTTPS metadata, unconditional HSTS emission violates the standard and can lock developers out of local HTTP testing.

**Suggested fix:** Gate HSTS injection on request transport security (checking `req.uri().scheme()` or `X-Forwarded-Proto: https`), or provide an option to disable HSTS in development.

## Positive notes

- `#![forbid(unsafe_code)]` remains strictly enforced on the core crate (`crates/core/src/lib.rs:7`).
- Prior findings S-M1 (path traversal sandboxing via canonicalization) and S-M2 (secrecy `SecretBox` storage) have been thoroughly and cleanly implemented.
- S-M4 (information leakage via `AppError`) was completely eliminated through adoption of RFC 9457 Problem Details (`application/problem+json`), ensuring internal diagnostic details are kept in server logs.
- CLI inspection commands (`env check` and `config check`) default to redacting secret keys and credential URLs, requiring an explicit `--unredacted` flag to display secrets.
- `safe_equal` utilizes `std::hint::black_box` to prevent compiler optimization of the constant-time accumulator loop.
- Default HTTP security headers provide a comprehensive modern defense baseline (CSP, HSTS, X-Frame-Options: DENY, nosniff, Referrer-Policy, Permissions-Policy).
- Declarative authorization policies (`RequireRole`, `RequirePermission`, `CustomPolicy`) cleanly separate authentication identity extraction from permission enforcement.

## Verdict

**Not ready to ship as-is without a fix pass.** The resolution of v01 security findings is verified and robust, but the newly introduced enterprise, multi-tenant, and authentication surface introduces 2 Critical findings and 4 Major findings. The SAML parser (C1) hardcodes expiration timestamps to `None` and lacks XML signature verification, enabling assertion replay and signature-free identity spoofing. The multi-tenant cache (C2) suffers from delimiter collisions that permit cross-tenant cache poisoning and data leakage. The subdomain tenant resolver (M1) allows rogue domain suffix-matching, and `HeaderTenantResolver` (M2) allows tenant hijacking via ID/slug collisions. In addition, API key storage (M3) and CSRF token issuance (M4) require hardening before production use.
