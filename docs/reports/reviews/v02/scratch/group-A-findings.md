# Group A — Multi-Tenant Boundary & Protocol Integrity (Findings)

Extracted from `docs/reports/reviews/v02/todo.md` and `docs/reports/reviews/v02/security.md`.

---

## Verbatim TODO Extraction

### Group A — Multi-Tenant Boundary & Protocol Integrity (solve together)
> Root cause: Delimiter collisions and unanchored suffix matching allow cross-tenant spoofing and cache contamination.

- [ ] **Security C2 — Multi-tenant cache key delimiter collision permits cross-tenant cache poisoning** (`security.md` C2).
      `crates/core/src/tenant/cache.rs:23–29`: Scoped keys use naive format `tenant:{tenant_id}:{key}`; tenant ID `"a:b"` with key `"c"` produces `tenant:a:b:c`, colliding with tenant `"a"` and key `"b:c"`. Disallow `:` in tenant IDs and keys or length-prefix/escape delimiters.
- [ ] **Security M1 — Subdomain tenant resolver permits suffix-matching domain spoofing** (`security.md` M1).
      `crates/core/src/tenant/resolver.rs:126–140`: `host_no_port.strip_suffix(&self.base_domain)` permits `evilapp.platform.io` to match `app.platform.io`. Require strict `.` separator (`.{base_domain}`) when matching subdomains.
- [ ] **Security M2 — `HeaderTenantResolver` shares a single map for IDs and slugs** (`security.md` M2).
      `crates/core/src/tenant/resolver.rs:47–53`: Registering both ID and slug into one `BTreeMap` allows an attacker tenant whose slug matches another tenant's ID to hijack requests. Separate ID and slug lookups into distinct maps or qualify keys (`id:`, `slug:`).
- [ ] **Security C1 — SAML 2.0 assertion parser hardcodes expiration timestamps and lacks signature verification** (`security.md` C1).
      `crates/core/src/enterprise/saml.rs:149–155`: `SamlParser::parse` hardcodes `not_before: None` and `not_on_or_after: None`, leaving assertions perpetually valid regardless of XML conditions. Parse `NotBefore` and `NotOnOrAfter` from `<saml2:Conditions>` and verify XML digital signatures.

---

## Verbatim Security Lens Report Details (`security.md`)

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

---

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

---

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

---

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

---

## Current Tree Inspection & Re-check Status

1. **Security C2 (`crates/core/src/tenant/cache.rs:23–29`)**:
   - Current code: Lines 23–29 implement `scoped_key` as `format!("tenant:{tenant_id}:{key}")` and `tenant_tag` as `format!("tenant:{tenant_id}")`. No delimiter escaping, colon rejection, or length prefixing is present.
   - Status: **holds**

2. **Security M1 (`crates/core/src/tenant/resolver.rs:133–149`)**:
   - Current code: Subdomain resolution strips suffix with `host_no_port.strip_suffix(&self.base_domain)` without requiring a leading dot separator (`.{base_domain}`) or validating single subdomain label constraints.
   - Status: **holds**

3. **Security M2 (`crates/core/src/tenant/resolver.rs:50–56`)**:
   - Current code: `HeaderTenantResolver::add_tenant` inserts both `tenant.id` and `tenant.slug` into `self.tenants` (`Arc<RwLock<BTreeMap<String, Tenant>>>`) without namespacing or collision checking.
   - Status: **holds**

4. **Security C1 (`crates/core/src/enterprise/saml.rs:147–156`)**:
   - Current code: `SamlParser::parse` hardcodes `not_before: None` and `not_on_or_after: None`. No XML signature verification or `<saml2:Conditions>` parsing exists.
   - Status: **holds**
