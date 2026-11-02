# Plan 01 — Multi-Tenant Boundary & Protocol Integrity

## Goal

Close all four critical and major security findings in Group A from [`docs/reports/reviews/v02/todo.md`](file:///home/skull/Projects/rume/docs/reports/reviews/v02/todo.md#L19-L30) to establish strict protocol and boundary integrity across multi-tenant routing, caching, and enterprise identity:

1. **Security C2**: Disambiguate multi-tenant cache keys and invalidation tags in [`rume/src/tenant/cache.rs`](file:///home/skull/Projects/rume/rume/src/tenant/cache.rs#L23-L29) via length-prefixing to eliminate cross-tenant cache poisoning and data leakage.
2. **Security M1**: Anchor base domain matching (`.{base_domain}`) and enforce single-label subdomain validation in [`rume/src/tenant/resolver.rs`](file:///home/skull/Projects/rume/rume/src/tenant/resolver.rs#L126-L153) to eliminate subdomain suffix-matching domain spoofing.
3. **Security M2**: Isolate tenant ID and tenant slug lookups into dedicated maps in [`rume/src/tenant/resolver.rs`](file:///home/skull/Projects/rume/rume/src/tenant/resolver.rs#L28-L91) to eliminate cross-tenant registration overwrites and traffic hijacking.
4. **Security C1**: Parse `<saml2:Conditions>` `NotBefore` and `NotOnOrAfter` temporal validity constraints, strip XML comments, and introduce an extensible signature verification hook in [`rume/src/enterprise/saml.rs`](file:///home/skull/Projects/rume/rume/src/enterprise/saml.rs#L48-L201).
5. Comprehensive regression and boundary integration test suites proving all four vulnerabilities are closed in [`rume/tests/tenant_suite.rs`](file:///home/skull/Projects/rume/rume/tests/tenant_suite.rs) and [`rume/tests/enterprise_suite.rs`](file:///home/skull/Projects/rume/rume/tests/enterprise_suite.rs).

## Current state

*Read before executing.*

- [`rume/src/tenant/cache.rs`](file:///home/skull/Projects/rume/rume/src/tenant/cache.rs#L23-L29): `scoped_key` concatenates `tenant:{tenant_id}:{key}` and `tenant_tag` concatenates `tenant:{tenant_id}` with raw colon delimiters without escaping or length prefixes. Tenant `"a:b"` with key `"c"` collides identically with tenant `"a"` and key `"b:c"`.
- [`rume/src/tenant/resolver.rs`](file:///home/skull/Projects/rume/rume/src/tenant/resolver.rs#L133-L149): `SubdomainTenantResolver` uses unanchored `host_no_port.strip_suffix(&self.base_domain)` without enforcing a leading dot boundary (`.{base_domain}`) or validating that the extracted subdomain is a single valid DNS label, permitting `evilapp.platform.io` to spoof `app.platform.io`.
- [`rume/src/tenant/resolver.rs`](file:///home/skull/Projects/rume/rume/src/tenant/resolver.rs#L49-L56): `HeaderTenantResolver::add_tenant` registers both `tenant.id` and `tenant.slug` into a single flat `BTreeMap<String, Tenant>`. Registering a tenant whose ID matches an existing tenant's slug silently overwrites the slug entry.
- [`rume/src/enterprise/saml.rs`](file:///home/skull/Projects/rume/rume/src/enterprise/saml.rs#L147-L156): `SamlParser::parse` hardcodes `not_before: None` and `not_on_or_after: None`, disabling temporal validation in `SamlParser::validate`. Ad-hoc substring search without comment stripping is vulnerable to comment injection, and no signature verification hook exists.
- [`rume/tests/tenant_suite.rs`](file:///home/skull/Projects/rume/rume/tests/tenant_suite.rs) and [`rume/tests/enterprise_suite.rs`](file:///home/skull/Projects/rume/rume/tests/enterprise_suite.rs): Existing test suites verify only happy-path scenarios and omit boundary collision, domain spoofing, and expired assertion tests.

## Global constraints

- Implementation defaults to `main` ([`.opencode/rules/30-git.md`](file:///home/skull/Projects/rume/.opencode/rules/30-git.md#L11-L12)).
- No commits without explicit authorization from the user ([`.opencode/rules/30-git.md`](file:///home/skull/Projects/rume/.opencode/rules/30-git.md#L3-L6)).
- Zero clippy warnings across workspace targets: `cargo clippy --workspace --all-targets` must pass cleanly ([`.opencode/rules/80-quality-bar.md`](file:///home/skull/Projects/rume/.opencode/rules/80-quality-bar.md#L27-L31)).
- Zero formatting diffs: `cargo fmt --all --check` must pass cleanly ([`.opencode/rules/80-quality-bar.md`](file:///home/skull/Projects/rume/.opencode/rules/80-quality-bar.md#L27-L31)).
- Verification claims require running the literal command and confirming actual output ([`.opencode/rules/40-verification.md`](file:///home/skull/Projects/rume/.opencode/rules/40-verification.md#L3-L11)).
- Follow the minimalism ladder: prefer existing codebase utilities and `std` before pulling in new dependencies ([`.opencode/rules/85-minimalism.md`](file:///home/skull/Projects/rume/.opencode/rules/85-minimalism.md#L8-L25)).
- TDD is the default: author failing regression tests first before implementing fixes ([`.opencode/rules/50-agent-conduct.md`](file:///home/skull/Projects/rume/.opencode/rules/50-agent-conduct.md#L17)).
- Document non-obvious engineering decisions at the decision site using `Why:` comments ([`.opencode/rules/70-code-rationale.md`](file:///home/skull/Projects/rume/.opencode/rules/70-code-rationale.md#L23-L36)).

## Constraints honoured

- Preserve existing public API signatures on [`TenantScopedCache`](file:///home/skull/Projects/rume/rume/src/tenant/cache.rs#L17-L66): `new`, `get`, `set`, `delete`, and `invalidate_tenant`.
- Preserve existing public API signatures on [`HeaderTenantResolver`](file:///home/skull/Projects/rume/rume/src/tenant/resolver.rs#L34-L57): `new`, `with_header`, and `add_tenant`.
- Preserve existing public API signatures on [`SubdomainTenantResolver`](file:///home/skull/Projects/rume/rume/src/tenant/resolver.rs#L100-L116): `new` and `add_tenant`.
- Preserve [`TenantResolver`](file:///home/skull/Projects/rume/rume/src/tenant/resolver.rs#L18-L25) trait definition and return types.
- Preserve [`Tenant`](file:///home/skull/Projects/rume/rume/src/tenant/model.rs#L14-L33), [`TenantContext`](file:///home/skull/Projects/rume/rume/src/tenant/model.rs#L39-L73), and [`OptionalTenant`](file:///home/skull/Projects/rume/rume/src/tenant/model.rs#L75-L99) extractor semantics.
- Preserve [`SamlAssertion`](file:///home/skull/Projects/rume/rume/src/enterprise/saml.rs#L27-L43) field names and types (`not_before: Option<u64>`, `not_on_or_after: Option<u64>`).
- Preserve [`SamlParser::validate`](file:///home/skull/Projects/rume/rume/src/enterprise/saml.rs#L159-L201) validation rules and [`SamlError`](file:///home/skull/Projects/rume/rume/src/enterprise/saml.rs#L7-L24) variants (`Expired`, `NotYetValid`, `AudienceMismatch`).
- Keep all existing tests in [`rume/tests/tenant_suite.rs`](file:///home/skull/Projects/rume/rume/tests/tenant_suite.rs) and [`rume/tests/enterprise_suite.rs`](file:///home/skull/Projects/rume/rume/tests/enterprise_suite.rs) passing without regressions.

## Task index

- `01-tenant-cache-delimiters.md` — Disambiguate tenant cache keys and invalidation tags via length-prefixing in `rume/src/tenant/cache.rs`.
- `02-tenant-resolvers-isolation.md` — Enforce strict leading-dot domain anchoring and single-label subdomains in `SubdomainTenantResolver`, and separate ID and slug lookup maps in `HeaderTenantResolver`.
- `03-saml-conditions-and-security.md` — Parse `NotBefore` and `NotOnOrAfter` conditions in `SamlParser`, strip XML comments, and add an extensible signature verification hook.
- `04-integration-tests.md` — Add comprehensive regression tests in `tenant_suite.rs` and `enterprise_suite.rs` verifying all four Group A vulnerabilities are closed.

## Done-when checklist

- [ ] Cache keys generated by `TenantScopedCache::scoped_key` are length-prefixed and delimiter collision-free between tenant IDs and keys.
- [ ] Invalidation tags generated by `TenantScopedCache::tenant_tag` are length-prefixed and collision-free.
- [ ] `SubdomainTenantResolver` rejects suffix domain spoofing (`evilapp.platform.io` matching `app.platform.io`) and rejects multi-label subdomains (`attacker.acme.app.platform.io`).
- [ ] `HeaderTenantResolver` stores IDs and slugs in isolated lookup maps, preventing registration collisions and traffic hijacking.
- [ ] `SamlParser::parse` extracts `NotBefore` and `NotOnOrAfter` epoch timestamps from `<saml2:Conditions>` or `<Conditions>` attributes, and strips XML comments before extracting tags.
- [ ] `SamlParser::validate` rejects assertions outside the temporal validity window with `SamlError::Expired` or `SamlError::NotYetValid`.
- [ ] `SamlParser` supports signature verification through a pluggable `SamlSignatureVerifier` trait.
- [ ] Integration tests in `rume/tests/tenant_suite.rs` and `rume/tests/enterprise_suite.rs` verify all four security vulnerabilities are closed.
- [ ] `cargo test -p rume --test tenant_suite --test enterprise_suite` passes with zero failures.
- [ ] `cargo clippy --workspace --all-targets` passes with zero warnings.
- [ ] `cargo fmt --all --check` passes with zero formatting issues.

## Verification commands

```bash
cargo test -p rume --test tenant_suite --test enterprise_suite
cargo clippy --workspace --all-targets
cargo fmt --all --check
```
