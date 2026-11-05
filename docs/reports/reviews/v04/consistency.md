# Cross-Lens Consistency Pass — Rume Whole-Document Review (v04)

Conductor review synthesis performed across all 10 completed lens reports in `docs/reports/reviews/v04/`.
This document reconciles cross-lens duplicate findings, resolves architectural contradictions, maps cross-subsystem dependencies, assigns unique ownership and concrete acceptance criteria for all Critical (C) and Major (M) findings, and establishes the dependency-respecting remediation order for `todo.md`.

---

## 1. Cross-Lens Duplicates & Synthesis

Multiple lenses independently audited interacting surfaces and surfaced overlapping defects. Per §6 rule 1, these findings are explicitly cross-referenced and given a single canonical owner rather than silently merged.

### 1.1 Premature Startup Traffic via `/health/ready` (Routing C1 & App Lifecycle M5/m4)
- **Reports**:
  - `routing-api-openapi.md` **C1**: Readiness probe returns 200 OK during startup (`AvailabilityState::Live`), routing traffic before modules and startup hooks complete (`crates/core/src/routing/health.rs:327–358`).
  - `app-lifecycle.md` **M5**: `Availability::set` allows unconstrained state mutation, and `state.rs:163` matches `AvailabilityState::Live` alongside `Ready` as healthy.
  - `app-lifecycle.md` **m4**: `/health/live` returns 200 OK for `AvailabilityState::Unknown`.
- **Synthesis & Canonical Owner**:
  - **Canonical Finding**: `routing-api-openapi.md` **C1** (`Health / Availability startup readiness defect`).
  - **Reconciliation**: In `routing/health.rs:335`, the readiness check explicitly treats `AvailabilityState::Live` as ready: `state == AvailabilityState::Ready || state == AvailabilityState::Live`. However, `builder.rs:1145` transitions to `Live` *before* module `on_start` hooks and before application `on_ready` hooks execute. Consequently, Kubernetes/load-balancers poll `/health/ready`, receive 200 OK, and route ingress traffic while database pools, background workers, or caches are still initializing.
  - **Resolution**: `/health/ready` must return 200 OK *only* for `AvailabilityState::Ready`. `AvailabilityState::Live` must report 503 Service Unavailable ("starting up"). `AvailabilityState::Unknown` must report 503 for both liveness and readiness.

### 1.2 Eager OpenAPI Route Freezing Omitting Module Routes (App Lifecycle M1 & Routing M1)
- **Reports**:
  - `app-lifecycle.md` **M1**: `RumeBuilder::openapi()` freezes route registry eagerly at definition time, and `run()` drops module routes from `serve_ctx` (`builder.rs:739–746, 1088–1091`).
  - `routing-api-openapi.md` **M1**: Eager OpenAPI generation in `Router::openapi()` captures a static snapshot at registration time, discarding all subsequent routes and merged subrouters (`router.rs:361–369`).
- **Synthesis & Canonical Owner**:
  - **Canonical Finding**: `app-lifecycle.md` **M1** (`OpenAPI route registry freezing & module route omission`).
  - **Reconciliation**: `Router::openapi()` compiles `OpenApiSpec::from_routes` eagerly when `.openapi(...)` is called on the builder or router. Any routes registered after that call, as well as all routes contributed by `Module::routes()`, are completely omitted from `/openapi.json`.
  - **Resolution**: Defer OpenAPI generation to preflight completion (`build_router` / `run`) using a lazy provider or dynamic registry snapshot that reflects the fully-merged router.

### 1.3 Module Route Preflight Collisions and Axum `ANY` Panics (App Lifecycle M4 & Architecture M1)
- **Reports**:
  - `app-lifecycle.md` **M4**: Module route collision preflight ignores `ANY` matcher overlap, triggering unhandled Axum runtime panics (`builder.rs:964–984`).
  - `architecture-module-boundaries.md` **M1**: Preflight verification fails to detect transitive collision hazards before Axum router merge.
- **Synthesis & Canonical Owner**:
  - **Canonical Finding**: `app-lifecycle.md` **M4**.
  - **Reconciliation**: `RumeBuilder` preflight checks `(method, path)` equality, but if a module registers `MethodFilter::all()` (or `ANY`), and another module registers `POST /path`, the preflight passes because methods do not string-equal, but Axum's underlying `Router::merge` panics on overlapping method sets.
  - **Resolution**: Check bitwise intersection of `MethodFilter` sets across paths during preflight; fail closed with `BuildError::DuplicateRoute`.

### 1.4 `quick-xml` Dependency Scope (Architecture M2 & Security Protocols M3)
- **Reports**:
  - `architecture-module-boundaries.md` **M2**: Core crate unconditionally depends on `quick-xml` for enterprise SAML parsing, violating core crate boundary constraints.
  - `security-protocols.md` **M3**: `SamlParser` needs `quick-xml` features for enveloped document signatures (`URI=""`).
- **Synthesis & Canonical Owner**:
  - **Canonical Finding**: `architecture-module-boundaries.md` **M2**.
  - **Reconciliation**: `quick-xml` is currently an unconditional dependency in `crates/core/Cargo.toml`, even though SAML 2.0 is an enterprise capability. It should be feature-gated under a dedicated `saml` (or `enterprise`) feature flag, keeping core `rume` minimal.

---

## 2. Contradiction Analysis & Rulings

### Contradiction 1: Enveloped Document Signatures (`URI=""`) vs Reference Strictness
- **Issue**:
  - `v03` and `v04` Lens 1/4 enforced strict matching: `Reference URI` must match `assertion_id` or `response_id`.
  - Lens 5 (`security-protocols.md` M3) points out that standard XMLDSig enveloped signatures on root SAML `<Response>` documents often use `URI=""` (RFC 3275 §4.3.3.2), which `SamlParser` currently rejects as `SignatureWrappingDetected`.
- **Ruling**:
  - `URI=""` represents the enclosing XML document. When a signature is present on the root `<Response>` element and carries `URI=""` with an Enveloped Signature Transform (`http://www.w3.org/2000/09/xmldsig#enveloped-signature`), this is a valid SAML protocol pattern and must be accepted if the assertion is a direct child of that response envelope.
  - Arbitrary URI references (e.g. `#_forged_id`) must remain strictly rejected as XSW attacks.

### Contradiction 2: Wall-Clock Sleeps in Testing vs Concurrency Determinism
- **Issue**:
  - Lens 1, 2, 3 reports noted that some container tests use 2,400ms sleeps to outlast integer-second lease durations (`UNIX_TIMESTAMP()`).
  - Lens 10 (`testing-feature-matrix.md` M1) flags that 33 wall-clock `sleep()` calls across test suites add >12s idle latency and introduce race conditions on congested CI nodes.
- **Ruling**:
  - In-memory SQLite and unit/integration tests must NEVER use wall-clock `sleep()`; they must use `tokio::sync::Notify`, atomic barriers, or Tokio's paused virtual time (`tokio::time::pause()`).
  - In container tests where database engines evaluate `UNIX_TIMESTAMP()` (1-second granularity), tests testing lease expiration can use minimal integer-second bounds, but long sleeps in tests where notification channels or polling loops can be used must be replaced with polling barriers (`tokio::time::timeout` + condition polling).

---

## 3. Cross-Lens Dependencies

1. **Readiness Probe (Routing C1) unblocks Graceful Drain (App Lifecycle M5)**:
   - Correcting `/health/ready` to only report 200 OK during `AvailabilityState::Ready` prevents premature ingress during boot and ensures load-balancers pull the node out of service immediately when `AvailabilityState::RefusingTraffic` is set during graceful drain.
2. **OpenAPI Lazy Registry (App Lifecycle M1) unblocks OpenAPI Spec Fidelity (Routing M1 & M2)**:
   - Routing and parameter validation regex matching for OpenAPI cannot be verified end-to-end until the builder exposes the complete router to the OpenAPI generator.
3. **`ArchitectureRules` Verification (Architecture M1) unblocks Module Testing Harness (Testing M2)**:
   - Upgrading `ModuleHarness` and `RumeTestHarness` depends on stable module boundary rules that correctly trace transitive dependencies and route overlaps.
4. **`CsrfConfig` Fail-Closed Startup (Security M1) unblocks Security Headers (Security m1)**:
   - CSRF secret configuration must be validated at application preflight so security middleware initializes safely.

---

## 4. Ownership & Acceptance Criteria for Critical (C) and Major (M) Findings

| ID | Title | Owning Lens | File & Target | Concrete Acceptance Criterion |
|---|---|---|---|---|
| **C1** | Startup premature traffic on `/health/ready` | Routing & OpenAPI | `crates/core/src/routing/health.rs:327` | `/health/ready` returns 503 while state is `Live`; returns 200 OK strictly when state is `Ready`. |
| **C2** | Multi-line secret leakage in CLI `config check` | CLI & DX | `crates/cli/src/commands/config.rs:114` | Multi-line secrets (RSA/PEM keys) are fully redacted across all continuation lines in human-readable output. |
| **M1** | Eager OpenAPI freezes route registry | App Lifecycle | `crates/core/src/app/builder.rs:739` | All routes (including module routes and post-declaration routes) appear in `/openapi.json`. |
| **M2** | Insecure defaults in `CsrfConfig` | Security & Protocols | `crates/core/src/security/csrf.rs:56` | Unauthenticated token mode requires explicit opt-in; missing HMAC secret in production config fails preflight. |
| **M3** | Subdomain cookie-tossing DoS in sessions | Security & Protocols | `crates/core/src/auth/session.rs:67` | `SessionScheme` evaluates all matching cookie headers/values rather than aborting on first invalid cookie. |
| **M4** | SAML enveloped document signatures and missing `NotOnOrAfter` | Security & Protocols | `crates/core/src/enterprise/saml.rs:147` | Enveloped signatures (`URI=""`) accepted; assertions missing `NotOnOrAfter` fail closed in `validate()`. |
| **M5** | `SqlAuditLogger` unparameterized queries & hardcoded PG DDL | Security & Protocols | `crates/core/src/security/audit.rs:76` | Audit logger uses parameterized queries and supports MySQL and SQLite schemas. |
| **M6** | Health check probe thundering herd race | Routing & OpenAPI | `crates/core/src/routing/health.rs:186` | Cache stampede protection (`singleflight`) wraps background probe execution during cache expiration. |
| **M7** | Module route preflight ignores `ANY` overlap | App Lifecycle | `crates/core/src/app/builder.rs:964` | Overlapping method bitsets on identical paths fail preflight with `BuildError::DuplicateRoute`. |
| **M8** | TestApp lifecycle teardown inverts shutdown hooks | App Lifecycle | `crates/core/src/app/builder.rs:602` | `build_test_app()` executes application shutdown hooks before module hooks, matching `run()` parity. |
| **M9** | Startup hooks block shutdown signals | App Lifecycle | `crates/core/src/app/builder.rs:1157` | Startup hook execution monitors SIGINT/SIGTERM and cancels cleanly if shutdown is signaled during boot. |
| **M10** | `Availability::set` allows unconstrained drain bypass | App Lifecycle | `crates/core/src/app/state.rs:159` | `Availability` rejects transitions back to `Ready` once `RefusingTraffic` or `Terminating` is set. |
| **M11** | `ArchitectureRules::verify` ignores undeclared dependencies | Architecture | `crates/core/src/module/architecture.rs:84` | Verification traverses transitive dependencies and fails on cycles and undeclared dependencies. |
| **M12** | Core crate unconditionally depends on `quick-xml` | Architecture | `crates/core/Cargo.toml:46` | `quick-xml` is gated behind an optional `saml` / `enterprise` feature flag. |
| **M13** | Crate root `lib.rs` violates re-export discipline | Architecture | `crates/core/src/lib.rs:35` | Domain-specific internal types removed from `rume::*` root; exposed via subsystem modules. |
| **M14** | `RateLimitService` reads missing tenant extension | Coordination | `crates/core/src/resilience/rate_limiter.rs:64` | `RateLimitService` extracts `TenantContext` using standard typed extractor. |
| **M15** | Redis cache default prefix `rume:` collides with other keys | Coordination | `crates/core/src/cache/redis.rs:88` | Default cache prefix uses isolated namespace `rume:cache:`. |
| **M16** | `Cache::clear()` executes dangerous `FLUSHDB` | Coordination | `crates/core/src/cache/redis.rs:97` | `Cache::clear()` deletes keys matching the configured prefix only, avoiding full database flush. |
| **M17** | Tag-set `PEXPIRE` shortens tag lifetime | Coordination | `crates/core/src/cache/redis.rs:110` | Tag TTL uses `MAX` logic or refreshes to longest constituent key TTL. |
| **M18** | `TenantScopedCache::invalidate_tag` releases quota for oldest keys | Coordination | `crates/core/src/tenant/cache.rs:139` | Quota release directly tracks the keys deleted by the tag invalidation. |
| **M19** | PostgreSQL leader `try_acquire` diverges on candidate ID | Coordination | `crates/core/src/coordination/postgres.rs:167` | Incumbent re-acquisition aligns with Redis semantics and does not self-fence. |
| **M20** | Tenant registration admits silent collisions | Coordination | `crates/core/src/tenant/resolver.rs:188` | Duplicate tenant IDs, slugs, or subdomains fail closed at registration. |
| **M21** | `remember_with_tags` stampede waiters can deadlock | Coordination | `crates/core/src/cache/typed.rs:208` | `Notify` registration happens before checking in-flight status, preventing missed wakeups. |
| **M22** | `rume config explain` outputs plaintext secrets | CLI & DX | `crates/cli/src/commands/config.rs:14` | Secrets and credential URLs are masked in `config explain` unless `--unredacted` is supplied. |
| **M23** | `rume doctor` port check reports false-positive | CLI & DX | `crates/cli/src/commands/doctor.rs:45` | Port check properly distinguishes successful bind from `"port in use"` error string. |
| **M24** | CLI component generators accept invalid Rust identifiers | CLI & DX | `crates/cli/src/commands/new.rs:88` | Generator validates input against Rust identifier rules (`[a-zA-Z_][a-zA-Z0-9_]*`). |
| **M25** | Non-transactional CLI component generation leaves orphans | CLI & DX | `crates/cli/src/commands/new.rs:120` | File generation is atomic; failures clean up newly created files. |
| **M26** | Interactive CLI prompts hang or swallow Ctrl+C in non-TTY | CLI & DX | `crates/cli/src/prompts.rs:25` | Prompts detect `is_terminal()`; fail closed with error in headless non-interactive environments. |
| **M27** | Severe test determinism regression (33 wall-clock sleeps) | Testing & Matrix | `crates/core/tests/` | Replace arbitrary test `sleep()` with deterministic barriers (`Notify`, polling timeout). |
| **M28** | Orphaned `RumeTestHarness` with zero adoption & compiler warnings | Testing & Matrix | `crates/core/src/testing/harness.rs:12` | Clean up warnings and integrate `RumeTestHarness` into integration test suites. |
| **M29** | Tautological Wiremock test in enterprise integration | Testing & Matrix | `crates/core/tests/integration/enterprise.rs:300` | Wiremock test asserts actual Rume IdP metadata resolution client. |
| **M30** | Missing negative-path and failure-mode test coverage | Testing & Matrix | `crates/core/tests/` | Add test coverage for event listener panics, saga compensation failures, and lease fences. |

---

## 5. Dependency-Respecting Remediation Order

Remediation must be organized into four logical tiers ordered by unblocking dependencies:

1. **P0 — Critical Availability, Security & Secret Protection**:
   - Fix `/health/ready` premature routing before boot completes (C1).
   - Fix multi-line secret leakage in CLI `config check` (C2).
   - Fix `CsrfConfig` insecure default and unauthenticated token validation (M2).
   - Fix `SessionScheme` subdomain tossing DoS (M3).
2. **P1 — Core Architecture, App Lifecycle & Coordination Integrity**:
   - Fix `RumeBuilder::openapi()` route freezing and lazy registration (M1).
   - Fix `ModuleRoute` preflight collision with `ANY` filter (M7).
   - Fix `Availability::set` monotonic lifecycle state machine (M10).
   - Fix `ArchitectureRules::verify` transitive dependency and cycle detection (M11).
   - Fix `RateLimitService` tenant extractor (M14).
   - Fix `Cache::clear()` destructive `FLUSHDB` (M16) and Redis namespace collision (M15).
   - Fix `remember_with_tags` waiter deadlock (M21).
   - Fix SAML enveloped document signatures and missing `NotOnOrAfter` (M4).
3. **P2 — CLI Hardening, Audit Persistence & Tenant Isolation**:
   - Parameterize `SqlAuditLogger` for MySQL and SQLite (M5).
   - Fix `rume config explain` secret masking (M22) and `doctor` port check false-positives (M23).
   - Validate CLI generator identifiers (M24) and atomic generation rollback (M25).
   - Add non-TTY detection to CLI prompts (M26).
   - Fix Tag TTL expiration math (M17) and tenant quota tracking (M18).
   - Fix tenant registration collision enforcement (M20).
   - Fix TestApp lifecycle shutdown hook ordering (M8) and startup signal cancellation (M9).
   - Gate `quick-xml` behind feature flag (M12) and prune `lib.rs` root re-exports (M13).
4. **P3 — Test Suite Determinism, Harness Cleanup & Minor Polish**:
   - Eliminate wall-clock sleeps across integration and container tests (M27).
   - Clean up `RumeTestHarness` warnings and adopt in test suites (M28).
   - Replace tautological wiremock test with real IdP metadata integration (M29).
   - Add negative-path assertions for event listeners, sagas, and worker fences (M30).
   - Resolve all minor (`m1`–`m6`) findings across all lenses.
