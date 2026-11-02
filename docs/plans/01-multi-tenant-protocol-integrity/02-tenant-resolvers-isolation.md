# Task 02 — Tenant Resolvers Isolation & Suffix Anchoring

## Goal

Prevent subdomain spoofing via unanchored suffix matching in `SubdomainTenantResolver` and prevent tenant hijacking via ID/slug collisions in `HeaderTenantResolver` in [`rume/src/tenant/resolver.rs`](file:///home/skull/Projects/rume/rume/src/tenant/resolver.rs).

## Files touched

- [`rume/src/tenant/resolver.rs`](file:///home/skull/Projects/rume/rume/src/tenant/resolver.rs)

## Deliverable

1. **Anchored subdomain matching in `SubdomainTenantResolver`**:
   - Normalize `base_domain` upon initialization or matching by ensuring a leading dot (e.g. `format!(".{}", base_domain.trim_start_matches('.'))`), ensuring that `host_no_port.strip_suffix(&dot_base_domain)` anchors strictly to a DNS label boundary.
   - Enforce single-label subdomain validation: extracted subdomains containing internal dots (`.`), empty strings, or non-alphanumeric/non-hyphen characters must be rejected and must not resolve to any tenant.
2. **Isolated lookup namespaces in `HeaderTenantResolver`**:
   - Replace the single shared `tenants: Arc<RwLock<BTreeMap<String, Tenant>>>` with separate identifier and slug maps: `by_id: Arc<RwLock<BTreeMap<String, Tenant>>>` and `by_slug: Arc<RwLock<BTreeMap<String, Tenant>>>`.
   - Update `HeaderTenantResolver::add_tenant` to insert into `by_id` and `by_slug` independently, preventing a tenant ID from overwriting an existing tenant's slug.
   - Update `HeaderTenantResolver::resolve` to look up by ID first, falling back to slug lookup only if not found in `by_id`.
3. Add a unit test module `#[cfg(test)] mod tests` in [`rume/src/tenant/resolver.rs`](file:///home/skull/Projects/rume/rume/src/tenant/resolver.rs) verifying spoofing rejection and namespace isolation.

## Constraints honoured

- Preserve existing public API signatures on [`HeaderTenantResolver`](file:///home/skull/Projects/rume/rume/src/tenant/resolver.rs#L29-L57): `new`, `with_header`, and `add_tenant`.
- Preserve existing public API signatures on [`SubdomainTenantResolver`](file:///home/skull/Projects/rume/rume/src/tenant/resolver.rs#L95-L116): `new` and `add_tenant`.
- Preserve [`TenantResolver`](file:///home/skull/Projects/rume/rume/src/tenant/resolver.rs#L18-L25) trait implementation signature and semantics.
- Preserve existing tests in [`rume/tests/tenant_suite.rs`](file:///home/skull/Projects/rume/rume/tests/tenant_suite.rs#L8-L136): `test_header_tenant_resolver_success_and_context_extractor`, `test_tenant_missing_and_unknown_rejections`, `test_subdomain_tenant_resolver`, and `test_optional_tenant_extractor`.
- Comply with [`.opencode/rules/70-code-rationale.md`](file:///home/skull/Projects/rume/.opencode/rules/70-code-rationale.md#L23-L36) with `Why:` comments at normalization and map lookup decision sites.
- Closes Security M1 and Security M2 findings from [`docs/reports/reviews/v02/todo.md:24-27`](file:///home/skull/Projects/rume/docs/reports/reviews/v02/todo.md#L24-L27) and [`docs/reports/reviews/v02/scratch/group-A-findings.md:49-105`](file:///home/skull/Projects/rume/docs/reports/reviews/v02/scratch/group-A-findings.md#L49-L105).

## Named tests (bodies omitted)

- `subdomain_resolver_rejects_unanchored_domain_spoofing`: configured with base domain `"app.platform.io"`; asserts host `"evilapp.platform.io"` resolves to `Ok(None)` and does not match tenant `"evil"`.
- `subdomain_resolver_rejects_multi_level_subdomains`: configured with base domain `"app.platform.io"`; asserts host `"attacker.acme.app.platform.io"` resolves to `Ok(None)` and does not match tenant `"attacker.acme"`.
- `subdomain_resolver_accepts_valid_single_label_subdomain`: configured with base domain `"app.platform.io"`; asserts host `"acme.app.platform.io"` resolves to `Ok(Some(tenant))`.
- `subdomain_resolver_handles_leading_dot_configuration_gracefully`: configured with base domain `".app.platform.io"` or `"app.platform.io"`; asserts consistent single-label resolution without double-dot anomalies.
- `header_resolver_prevents_slug_overwrite_by_colliding_id`: registers Tenant 1 (`id: "t-001"`, `slug: "acme"`) and Tenant 2 (`id: "acme"`, `slug: "globex"`); asserts that querying `"acme"` resolves Tenant 2 by ID, and querying `"t-001"` resolves Tenant 1 by ID.
- `header_resolver_slug_fallback_resolves_when_no_id_conflict`: registers Tenant 1 (`id: "t-001"`, `slug: "acme"`); asserts that querying header `"acme"` falls back to slug lookup and successfully resolves Tenant 1.

## Code-only decisions

- Subdomain base domain normalization:
  ```rust
  // Why: Normalizing base_domain to guarantee a leading dot (".{base_domain}") ensures that
  // host.strip_suffix matches strictly on a DNS label boundary. Without the leading dot,
  // "evilapp.platform.io" matches suffix "app.platform.io", extracting "evil" as a valid tenant.
  let normalized_base = if self.base_domain.starts_with('.') {
      self.base_domain.clone()
  } else {
      format!(".{}", self.base_domain)
  };
  ```

- Single-label validation:
  ```rust
  // Why: A tenant subdomain must be a single DNS label. Subdomains containing dots
  // (e.g. "attacker.acme.app.platform.io") represent nested domain delegations that must
  // not be treated as a composite slug.
  if subdomain.is_empty() || subdomain.contains('.') || !subdomain.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
      return Ok(None);
  }
  ```

- Separate lookup maps in `HeaderTenantResolver`:
  ```rust
  // Why: Separate maps for ID and slug lookups prevent registration order from overwriting
  // entries across namespaces when Tenant A's slug matches Tenant B's ID.
  pub struct HeaderTenantResolver {
      header_name: String,
      by_id: Arc<RwLock<BTreeMap<String, Tenant>>>,
      by_slug: Arc<RwLock<BTreeMap<String, Tenant>>>,
  }
  ```

## Verification

```bash
cargo test -p rume tenant::resolver
cargo clippy -p rume --all-targets
```

Expected output:
- `test tenant::resolver::tests::subdomain_resolver_rejects_unanchored_domain_spoofing ... ok`
- `test tenant::resolver::tests::subdomain_resolver_rejects_multi_level_subdomains ... ok`
- `test tenant::resolver::tests::subdomain_resolver_accepts_valid_single_label_subdomain ... ok`
- `test tenant::resolver::tests::header_resolver_prevents_slug_overwrite_by_colliding_id ... ok`
- `test tenant::resolver::tests::header_resolver_slug_fallback_resolves_when_no_id_conflict ... ok`
- Clippy exits 0 with zero warnings.

## Commit

*Only when the user asks:*

```commit
fix(tenant): anchor subdomain matching and isolate header resolver maps

Anchor base domain matching with a leading dot, enforce single-label
subdomain validation, and isolate ID and slug lookup maps in
HeaderTenantResolver to prevent spoofing and tenant hijacking.
Closes Security M1 and Security M2.
```

## Done-when

- [ ] `SubdomainTenantResolver` rejects `evilapp.platform.io` when configured with `app.platform.io`.
- [ ] `SubdomainTenantResolver` rejects subdomains containing dots (`attacker.acme.app.platform.io`).
- [ ] `HeaderTenantResolver` maintains distinct `by_id` and `by_slug` maps.
- [ ] Registering Tenant 2 with ID matching Tenant 1's slug does not overwrite Tenant 1 in `by_slug`.
- [ ] All unit and integration tests in `rume/tests/tenant_suite.rs` pass with zero regressions.
