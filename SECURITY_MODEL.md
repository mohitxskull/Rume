# Rume — Threat Model

Repository: `mohitxskull/Rume` (pre-1.0 Axum application framework, v0.1.x). No separate infrastructure repository: Rume is a library, and deployment environments are out of scope per `SECURITY.md`. `workbench/multi-instance` ships Docker Compose files, used only as a reference deployment.

## System overview and trust boundaries

Rume is a library that other developers build HTTP apps with. The trust boundary sits between the **consuming app's developer/operator** (trusted: writes the code, config, secrets and migrations) and **remote parties who reach the consuming app**: HTTP clients, other tenants, and identity providers that send SAML/SCIM payloads. A Rume vulnerability is a flaw in its public APIs or documented defaults that lets one of those remote parties attack an app that uses Rume as intended.

## Components

| Component | Code | Notes |
|---|---|---|
| **Security** | `crates/security/` (auth schemes, sessions, policy layer, CSRF, headers, audit, tenant resolver/layer/cache/quota) | Main defence against untrusted requests |
| **HTTP** | `crates/http/` (router, registry, validation, OpenAPI, rate limiter, circuit breaker, health routes) | Request handling and client-IP derivation |
| **Enterprise** | `crates/enterprise/` (SAML assertion parsing/validation, SCIM models, Saga coordinator) | Experimental but in scope; parses IdP XML |
| **Data** | `crates/data/` (SQL backends, migrations, SQL validation via `sqlparser`, cache, Redis/Postgres locks and leader election) | Persistence and coordination |
| **Messaging** | `crates/messaging/` (event bus, transactional outbox, job queue/worker, idempotency store) | Background processing |
| **App & Foundation** | `crates/app/`, `crates/foundation/`, `crates/observability/`, `crates/core/`, `crates/macro/` | Builder, lifecycle, config/env/secrets loading, errors, `safe_equal`, telemetry |
| **CLI & Templates** | `crates/cli/` (incl. `templates/`) | Scaffolds new projects; generated code ships into user apps |
| **Workbench** | `workbench/simple-api`, `workbench/modular-monolith`, `workbench/multi-instance` | Reference apps; in scope per `SECURITY.md` |

Dev-only: `crates/testing`, `crates/crash`, `crates/xtask`, `fuzz/`.

## Entry points and who can reach them

- HTTP requests to consuming apps: headers (`Authorization`, `Cookie`, `X-Tenant-ID`, `Host`, `X-Forwarded-For`), paths and bodies. These go through `AuthLayer`, `PolicyLayer`, `CsrfLayer`, the tenant layers, the rate limiter and validation. **Reachable by anyone on the internet.**
- SAML responses (`SamlParser`, `SamlSignatureVerifier`) and SCIM payloads: shaped by an IdP or by an attacker replaying or forging them.
- Health/readiness/metrics routes (`crates/http/src/routing/health.rs`, observability exporters).
- Developer-side inputs (trusted): TOML/env config, secrets, migrations, SQL assets, CLI arguments. CLI-generated templates become code in user apps.

## Assets

- Authenticated identities: sessions, API keys, bearer tokens, CSRF secrets.
- Tenant data isolation: tenant-scoped cache keys, quotas, tenant resolution.
- Integrity of persisted data and migrations; exactly-once and idempotent processing of jobs and outbox messages.
- Secrets in config (`secrecy` redaction in logs and diagnostics).
- Availability: rate limiting, circuit breaker, bounded memory in caches, limiters and quota stores.

## Attacker profiles (maintainer-confirmed)

- **Unauthenticated internet client** of an app built on Rume.
- **Malicious tenant or authenticated user** trying to cross tenant boundaries or escalate privileges.
- **Forged or replayed SAML/SCIM payloads**: Enterprise is in scope nothing is out of scope.

All components matter equally; there are no extra priorities.

## Existing controls

- Constant-time comparison with `safe_equal` (`subtle`).
- SHA-256 hashing of API keys in `auth/schemes.rs`.
- HMAC-signed CSRF tokens, with an explicit `insecure_without_secret_for_testing` helper.
- Rate limiter honours `X-Forwarded-For` only from `trusted_proxies` and parses it right to left (v06 review).
- SAML checks Audience, Destination and mandatory `NotOnOrAfter`. Signature verification is delegated to a caller-supplied `SamlSignatureVerifier`.
- Fail-closed idempotency lock extension; session ID regeneration with tenant ID.
- CI runs `cargo audit`, CodeQL, fuzzing (`fuzz/` saml_parser) and strict clippy. Earlier audit remediation is recorded in `docs/plans/` and `docs/reports/reviews/`.

## Out of scope / accepted risk

- From `SECURITY.md`: bugs in third-party dependencies, and deployment environments or infrastructure that users operate themselves.
- Nothing else is ruled out: workbench apps, CLI/templates and experimental enterprise features are all in scope.
- Behaviour that needs a malicious developer or operator, or the explicit `insecure_*_for_testing` helpers.

## Open questions

- Whether a misconfiguration footgun (an insecure default a developer must opt out of) should rank the same as a direct vulnerability. Until the maintainer decides otherwise, the scanner's severity stands.
