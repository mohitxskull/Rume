# Task 04 — `rume-security`, `rume-http`, & `rume-enterprise`

## Goal

Create `rume-security` (`crates/security`: `auth`, `security`, `tenant`), `rume-http` (`crates/http`: `openapi`, `routing`, `validation`, `resilience`), and `rume-enterprise` (`crates/enterprise`: `enterprise`, `saga`), breaking `openapi <-> routing` and `routing <-> validation` cycles and wiring OTel span parent propagation into `Saga`.

## Files touched

- `Cargo.toml` (`[workspace.members]`: add `"crates/security"`, `"crates/http"`, `"crates/enterprise"`)
- `crates/security/Cargo.toml`, `crates/security/src/lib.rs`
- `crates/security/src/{auth, security, tenant}/` (moved from `crates/core/src/{auth, security, tenant}/`)
- `crates/http/Cargo.toml`, `crates/http/src/lib.rs`
- `crates/http/src/{openapi, routing, validation, resilience}/` (moved from `crates/core/src/{openapi, routing, validation.rs, resilience}/`; `health_routes` decoupled from `AppContext` via `Availability` + `HealthChecks` from `rume-foundation`)
- `crates/enterprise/Cargo.toml`, `crates/enterprise/src/lib.rs`
- `crates/enterprise/src/{enterprise, saga}/` (moved from `crates/core/src/{enterprise, saga}/`)

## Deliverable

1. `rume-security` depends on `rume-foundation` and `rume-data` (for `cache` in `tenant/cache.rs`, `persistence` in `security/audit.rs`, `redis` in `auth/redis.rs`).
2. `rume-http` depends on `rume-foundation`, `rume-observability`, and `rume-security` (for `Tenant`/`TenantContext` extension lookup in `resilience/rate_limiter.rs`). Internally `openapi` has zero back-edge to `routing` (accepting `RouteInfo` metadata slices/traits), and `routing`/`validation` share `RouteSpec` cleanly. Existing `#[cfg(test)]` unit tests in `routing/group.rs:220` and `validation.rs:323` that used `crate::testing::TestApp` are rewritten to use `tower::ServiceExt::oneshot` directly (or relocated to `rume-testing` / `crates/core/tests/`) so `rume-http` has zero dependency on `rume-testing`.
3. `rume-enterprise` depends on `rume-foundation` and `rume-observability`, gating `saml` (`quick-xml`, `saml-rs`) behind `feature = "saml"` / `feature = "enterprise"`. `SagaCoordinator` creates child `tracing::Span`s per step/compensation linked to the active OTel `Context`.

## Constraints honoured

- `crates/core/src/routing/health.rs`: `/health`, `/health/live`, `/health/ready` status codes (503 when `AvailabilityState::RefusingTraffic` or `HealthStatus::Failed`) preserved using `rume_foundation::state::Availability` and `rume_foundation::health::HealthChecks`.
- `crates/core/src/validation.rs`: RFC 9457 Problem Details rejection for `ValidatedJson`, `ValidatedQuery`, `ValidatedPath`, `ValidatedForm` preserved.
- `crates/core/src/enterprise/saml.rs`: `#[cfg(feature = "saml")]` experimental gating and `saml-rs = "=0.8.0"` delegation preserved.
- `crates/core/src/resilience/rate_limiter.rs`: Per-tenant rate limiting via `Tenant` / `TenantContext` request extensions preserved.

## Named tests (bodies omitted)

- `http_openapi_generates_spec_from_route_registry_without_routing_cycle` — asserts `generate_openapi` produces valid OpenAPI paths and schemas from `RouteRegistry` entries.
- `http_health_routes_return_503_when_refusing_traffic` — asserts readiness endpoint transitions to `503` when `Availability` is set to `RefusingTraffic`.
- `enterprise_saga_coordinator_links_step_and_compensation_spans_to_parent_trace` — asserts `SagaCoordinator` execution and reverse compensation emit `tracing::Span`s sharing the caller's OTel `trace_id`.

## Verification

```bash
cargo test -p rume-security -j 2
cargo test -p rume-http -j 2
cargo test -p rume-enterprise --features enterprise -j 2
```

Expected: All three crates compile with 0 warnings and their unit tests pass.

## Commit

Only when the user asks:

```text
refactor(security,http,enterprise): extract rume-security, rume-http, and rume-enterprise
```

## Done-when

- [ ] `crates/security/src/lib.rs`, `crates/http/src/lib.rs`, and `crates/enterprise/src/lib.rs` all declare `#![forbid(unsafe_code)]`.
- [ ] `openapi` module has no back-edge to `routing` or `validation`.
- [ ] `cargo test -p rume-security -j 2`, `cargo test -p rume-http -j 2`, and `cargo test -p rume-enterprise --features enterprise -j 2` pass.
