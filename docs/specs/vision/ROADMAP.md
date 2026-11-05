# Rume — Engineering Roadmap

> **This roadmap outlines the phased architectural evolution of Rume from an ergonomic application layer into a complete, modular Rust application framework built around progressive complexity.**

---

## Strategic Overview

Rume follows four development phases:
1. **Phase 1: Progressive Application Architecture** — Establish core composition, first-class modules, cycle detection, and topological lifecycle orchestration.
2. **Phase 2: Complete Application Capabilities** — Add first-party persistence, authentication, authorization, background execution, and application events.
3. **Phase 3: Enterprise Reliability** — Implement transactional outbox, job reliability, resilience middleware, unified observability, and architecture test verification.
4. **Phase 4: Framework Maturation** — Expand into advanced distributed processing, workflow orchestration, and enterprise ecosystem integrations.
5. **Phase 5: Production Semantics and Integration** — Transition from in-memory contracts to durable, multi-process production implementations with genuine persistence, transactional outbox atomicity, shared coordination, and comprehensive failure recovery.

---

## Phase 1 — Progressive Application Architecture

> **Priority:** P0 · Architectural Foundation  
> **Status:** Completed & Verified in `00c4252`

| ID | Task | Acceptance Criteria | Status |
|---|---|---|---|
| **ARCH-01** | Progressive Application Composition | Minimal application starts with 3 lines of code (`Rume::new()`); full configuration remains available via `Rume::builder()`. | **Completed** |
| **ARCH-02** | First-Class Application Modules | `Module` trait allows domains (`UsersModule`, `BillingModule`) to encapsulate private state, routes, and lifecycle. | **Completed** |
| **ARCH-03** | Scoped Module Context | `ModuleContext` provides scoped availability, lifecycle, and paths to modules without global state leakage. | **Completed** |
| **ARCH-04** | Explicit Module Dependencies | Modules declare dependencies via `dependencies()`; verified at preflight. | **Completed** |
| **ARCH-05** | Architectural Verification | Circular dependencies (`A -> B -> A` or deep DAG cycles) and missing dependencies fail closed at preflight with zero side effects. | **Completed** |
| **ARCH-06** | Topological Lifecycle Orchestration | Modules start in topological dependency order (`on_start`); shut down and roll back on failure in reverse topological order (`on_shutdown`). | **Completed** |
| **ARCH-07** | Module-Scoped Middleware & Health | Modules register domain-owned Tower layers and diagnostic health checks without global coupling. | **Completed** |

**Verification Suite:** [`crates/core/tests/progressive_composition.rs`](../../../crates/core/tests/progressive_composition.rs) (8/8 integration tests passing).

---

## Phase 2 — Complete Application Capabilities

> **Priority:** P1 · Framework Capabilities  
> **Status:** Completed & Verified

This phase transitions Rume into a complete application framework providing integrated capabilities by default while preserving architectural freedom.

| ID | Task | Acceptance Criteria | Target |
|---|---|---|---|
| **CAP-01** | Complete Typed Request Validation | JSON body, path params, query strings, headers, and form inputs share unified, sanitized RFC 9457 Problem Details rejection behavior. | **Completed** |
| **CAP-02** | First-Party Persistence Integration | Connection pool lifecycle management, automated migrations, health checks, and explicit transaction boundaries integrate seamlessly with Rume. | **Completed** |
| **CAP-03** | Authentication Foundation | Pluggable identity providers, secure session management, API key handling, and token authentication middleware. | **Completed** |
| **CAP-04** | Authorization Foundation | Declarative policies, role/permission checks, and explicit enforcement at handler and service boundaries. | **Completed** |
| **CAP-05** | Cache Integration | Typed cache interface with memory and Redis adapters, TTL caching, tag invalidation, and lifecycle integration. | **Completed** |
| **CAP-06** | Background Job Contracts | Typed background job declarations, execution runners, retry semantics, failure handling, and graceful worker drain during shutdown. | **Completed** |
| **CAP-07** | Application Events | Typed inter-module domain events with explicit synchronous dispatch and asynchronous delivery contracts. | **Completed** |
| **CAP-08** | API Schema Integration | Automatic OpenAPI / JSON Schema generation reflecting canonical route registry metadata and validated parameter types. | **Completed** |

**Verification Suites:**
- [`crates/core/tests/validation_suite.rs`](../../../crates/core/tests/validation_suite.rs)
- [`crates/core/tests/persistence_integration.rs`](../../../crates/core/tests/persistence_integration.rs)
- [`crates/core/tests/auth_suite.rs`](../../../crates/core/tests/auth_suite.rs)
- [`crates/core/tests/cache_suite.rs`](../../../crates/core/tests/cache_suite.rs)
- [`crates/core/tests/jobs_suite.rs`](../../../crates/core/tests/jobs_suite.rs)
- [`crates/core/tests/events_suite.rs`](../../../crates/core/tests/events_suite.rs)
- [`crates/core/tests/openapi_suite.rs`](../../../crates/core/tests/openapi_suite.rs)

---

## Phase 3 — Enterprise Reliability

> **Priority:** P1 · Scale and Maintainability  
> **Status:** Completed & Verified

This phase provides enterprise-grade reliability, data consistency, and architectural enforcement.

| ID | Task | Acceptance Criteria | Target |
|---|---|---|---|
| **ENT-01** | Transactional Outbox | Durable event publication pattern ensuring atomic database mutation and event emission without message loss on process crash. | **Completed** |
| **ENT-02** | Job Reliability | Distributed worker execution, idempotency keys, exponential backoff retries, dead-letter queues, and worker heartbeat recovery. | **Completed** |
| **ENT-03** | Resilience Middleware | Circuit breakers, adaptive timeouts, concurrency limits, rate limiting, and shed-load policies. | **Completed** |
| **ENT-04** | Unified Observability | Correlated W3C TraceContext distributed tracing across asynchronous boundaries, Prometheus metrics registry, structured logging, and RFC 9457 error tracking. | **Completed** |
| **ENT-05** | Module Integration Testing | In-memory and test-container test harnesses allowing individual modules to be tested with real dependencies or explicit test doubles. | **Completed** |
| **ENT-06** | Architecture CI Verification | Compile-time and CI test runners enforcing module visibility, forbidden cross-domain calls, and clean dependency graphs. | **Completed** |
| **ENT-07** | Multi-Tenant Primitives | Optional tenant resolution extractors, connection scoping, and tenant-isolated cache/storage boundaries. | **Completed** |
| **ENT-08** | Security Hardening | Hardened security headers, CSRF protection, constant-time comparisons, and audit log integration. | **Completed** |

**Verification Suites:**
- [`crates/core/tests/outbox_suite.rs`](../../../crates/core/tests/outbox_suite.rs)
- [`crates/core/tests/job_reliability_suite.rs`](../../../crates/core/tests/job_reliability_suite.rs)
- [`crates/core/tests/resilience_suite.rs`](../../../crates/core/tests/resilience_suite.rs)
- [`crates/core/tests/metrics_suite.rs`](../../../crates/core/tests/metrics_suite.rs)
- [`crates/core/tests/module_testing_and_architecture_suite.rs`](../../../crates/core/tests/module_testing_and_architecture_suite.rs)
- [`crates/core/tests/tenant_suite.rs`](../../../crates/core/tests/tenant_suite.rs)
- [`crates/core/tests/security_suite.rs`](../../../crates/core/tests/security_suite.rs)

---

## Phase 4 — Framework Maturation

> **Priority:** P2 · Long-Term Expansion  
> **Status:** Completed & Verified

Specialized enterprise and distributed extensions built on the proven core architecture and capability layers:

| ID | Task | Acceptance Criteria | Target |
|---|---|---|---|
| **MAT-01** | Workflow Orchestration | Multi-step saga execution with functional or trait steps, automated reverse compensation hooks, and full execution tracing. | **Completed** |
| **MAT-02** | Distributed Coordination | Exclusive distributed locking with monotonic fencing tokens and TTL auto-expiration; leader election with lease renewal. | **Completed** |
| **MAT-03** | Enterprise Protocol Starters | Standard SCIM 2.0 (RFC 7643 / RFC 7644) provisioning envelopes and experimental SAML 2.0 assertion parser (non-production; XMLDSig validation pending). | **Completed** |

**Verification Suites:**
- [`crates/core/tests/saga_suite.rs`](../../../crates/core/tests/saga_suite.rs)
- [`crates/core/tests/coordination_suite.rs`](../../../crates/core/tests/coordination_suite.rs)
- [`crates/core/tests/enterprise_suite.rs`](../../../crates/core/tests/enterprise_suite.rs)

---

## Phase 5 — Production Semantics and Integration

> **Priority:** P0 · Framework Integration & Correctness  
> **Status:** Complete / Production Verified

This phase ensures Rume's capabilities function dependably under realistic application workloads, multi-process concurrency, crashes, and failure recovery. It closes the critical distinction between in-memory testing implementations and durable production semantics.

### Strategic Tasks

| ID | Task | Acceptance Criteria | Target |
|---|---|---|---|
| **REL-01** | Real Persistence Foundation | Genuine database integration (PostgreSQL driver), real connection acquisition/release, min/max pool sizing, connection acquisition timeouts, explicit transaction isolation, rollback on drop/error, real health checks, and graceful active-transaction drainage. | **P0** |
| **REL-02** | Transactional Outbox Atomicity | Enlist outbox inserts directly into business database transactions (`BEGIN ... INSERT data ... INSERT outbox ... COMMIT`), guaranteeing atomic write/publish without message loss on crash; atomic claims across multiple relay processes with leases, expired claim recovery, exponential backoff, and strict drainage error propagation. | **P0** |
| **REL-03** | Durable Jobs & Worker Recovery | Persistent queue backend, atomic job claiming with leases and heartbeats, automatic abandoned-job recovery, durable retry scheduling, dead-letter storage, and cross-process idempotency. | **P1** |
| **REL-04** | Production Distributed Coordination | Real shared-store distributed lock and leader election with lease auto-renewal, node crash takeover, and monotonically increasing fencing tokens. | **P1** |
| **REL-05** | Production Infrastructure Integrations | Production Redis cache adapter and session store, multi-instance cache quota enforcement, and distributed tag invalidation. | **P1** |
| **REL-06** | Security & Enterprise Hardening | Standards-compliant XML parsing for experimental SAML 2.0 (flagged non-production until certified XMLDSig verification is implemented), audience/issuer/time-condition validation, and hardened tenant isolation. | **P1** |
| **REL-07** | Meaningful Architecture Verification | Clarify and enforce boundaries between declared module dependency graphs (`Module::dependencies()`) and compile-time Rust module visibility rules. | **P1** |

### Sprint Prioritization Order

| Order | Issue / Deliverable | Priority | Focus |
|---|---|---|---|
| **1** | Fix outbox relay error handling and drainage semantics | **P0** | Prevent false-positive drain completion on repo errors; propagate loop errors. |
| **2** | Replace simulated connection leasing with real persistence semantics | **P0** | Physical connection pool interface, timeout control, and real pool drain. |
| **3** | Design transaction-enlisted outbox storage | **P0** | Contract allowing outbox event inserts inside active database transactions. |
| **4** | Implement production database backend | **P0** | PostgreSQL adapter with real transaction and migration execution. |
| **5** | Implement durable outbox storage and recovery | **P0** | Atomic claim leases, expired claim re-drive, and crash recovery. |
| **6** | Implement persistent job queue and worker recovery | **P1** | Shared storage queue, heartbeat monitoring, and abandoned job reclamation. |
| **7** | Implement distributed locking and leader election backend | **P1** | Shared backend with monotonic lease tokens and multi-process takeover. |
| **8** | Harden SAML, authentication and tenant isolation | **P1** | XML signature verification, audience/time validation, and tenant isolation. |

### End-to-End Enterprise Integration Test Pipeline

The final milestone of Phase 5 is a comprehensive failure-oriented integration test verifying a realistic enterprise pipeline:
1. Authenticate request -> Resolve tenant -> Authorize business operation.
2. Begin database transaction -> Update business record + Append outbox event atomically -> Commit transaction.
3. Durable relay claims event -> Publishes to external bus -> Marks published.
4. Background worker executes resulting job with idempotency and heartbeat tracking.
5. Record metrics, structured logs, and tracing spans.
6. **Failure Injection:** Kill worker, crash relay post-commit, simulate database disconnect, expire leader lease, and initiate graceful shutdown—verifying data consistency, zero event loss, and clean recovery.

---

## Phase 6 — Stabilization through Real-Application Validation

> **Priority:** P0 · Real-Application Validation & Production Hardening  
> **Status:** Completed & Verified

This phase proves that Rume is genuinely easier to build and operate with—not just that its individual subsystems pass isolated tests. It builds and exercises a complete multi-tenant SaaS application (**Rume Workbench**) scaffolded directly from `rume new`, auditing developer experience and validating deployment under live concurrency.

### Strategic Tasks

| ID | Task | Acceptance Criteria | Target |
|---|---|---|---|
| **STAB-01** | Preflight Telemetry & Supervision | Correct pool telemetry to distinguish physical driver active connections from logical permits; enforce worker task supervision with awaitable drains preventing post-drain job dispatches; add relay health state and error visibility; keep SAML experimental. | **Completed** |
| **STAB-02** | Reference Application (Rume Workbench) | Scaffold from `rume new` and build multi-tenant project & issue management application: users, orgs, projects, issues, PostgreSQL persistence, migrations, and atomic transactions. | **Completed** |
| **STAB-03** | End-to-End Outbox & Background Processing | Atomic issue creation + `IssueCreatedEvent` enrollment in single DB transaction; outbox relay claims and dispatches to `SendNotificationJob`; worker executes with retry, DLQ, and Redis idempotency. | **Completed** |
| **STAB-04** | Developer Experience (DX) Audit & Polish | Remove friction points: relative path resolution in `rume new`, ergonomic `AppError::bad_request` and `AppError::not_found` constructors, unified `Migration::from_sql` builder, and public schema constants. | **Completed** |
| **STAB-05** | Multi-Instance Deployment Validation | Run two application instances concurrently with real PostgreSQL and Redis containers, verifying cross-tenant isolation, concurrent claiming via `SKIP LOCKED`, zero message duplicates, and graceful shutdown drain. | **Completed** |
| **STAB-06** | Phase 6.1 Corrective Closure & Security Hardening | Supervised OS signal lifecycle with `WorkerHandle::shutdown()` panic/cancellation inspection; strict `TenantMembershipLayer` rejecting cross-tenant impersonation with 403 Forbidden; persisted organizations and users with database-backed token authentication; isolated multi-pool deployment validation; tightened queue and outbox repository contracts. | **Completed** |
| **STAB-07** | Phase 6.2 Persistence DX & Safe Transactions | First-class parameterized transaction execution (`execute_params`, `query_scalar_params`, `params!`) across Postgres, MySQL, and SQLite with native `raw_tx()` driver escape hatches; separation of Workbench HTTP handlers from database persistence into repository assets; elimination of all raw SQL interpolation and manual quote escaping; versioned `.sql` migration files; core persistence schema ownership via `Migrator::with_rume_core()`; and fail-closed SHA-256 migration checksum validation. | **Completed** |
| **STAB-08** | Phase 6.3 First-Class SQL Assets & Database Developer Experience | Build-time SQL discovery and compile-time embedding macro (`rume::sql_assets!`) exposing typed `SqlQuery<'static>` constants; driver-independent generic row extraction (`DbRow`, `FromRow`, `FromDbValue`) and query execution (`fetch_all_as`, `fetch_optional_as`, `fetch_one_as`) across Postgres, MySQL, and SQLite without leaking `sqlx::Row`; convention-based compile-time migration discovery and pairing macro (`rume::embed_migrations!`); database and migration CLI commands (`rume make:migration`, `rume db:status`, `rume db:migrate`, `rume db:rollback`); and refactoring of Workbench repositories to eliminate direct driver dependencies. | **Completed** |

**Verification Suites:**
- [`workbench/multi-instance/tests/workbench_test.rs`](../../../workbench/multi-instance/tests/workbench_test.rs) (End-to-end multi-tenant CRUD, persisted auth, negative tenant authorization tests, transaction-enlisted outbox, worker processing, signal-based graceful shutdown drain, and SQL injection / Unicode immunity).
- [`workbench/multi-instance/tests/deployment_validation.rs`](../../../workbench/multi-instance/tests/deployment_validation.rs) (Multi-instance concurrency across tenants with independent connection pools, Redis idempotency deduplication, competing outbox relays, and worker shutdown supervision).
- [`crates/core/tests/integration/persistence.rs`](../../../crates/core/tests/integration/persistence.rs) (Preflight and runner checksum mismatch verification, core persistence schema registration via `with_rume_core`, and generic query asset execution via `SqlQuery` and `FromRow`).
- [`crates/cli/tests/generated_project.rs`](../../../crates/cli/tests/generated_project.rs) (CLI workflow test for `make:migration`, `db:status`, `db:migrate`, and `db:rollback`).

---

## Phase 7 — Production Release & Ecosystem Readiness

> **Priority:** P1 · Ecosystem Polish & Release Readiness  
> **Status:** Next Scheduled Phase

With runtime durability, multi-tenant isolation, safe transactions, and first-class SQL assets established and verified through Rume Workbench, Phase 7 focuses on final release readiness for the open-source ecosystem:

### Strategic Tasks

| ID | Task | Acceptance Criteria | Target |
|---|---|---|---|
| **REL-08** | Production Benchmarks & Load Testing | Benchmark throughput and latency overhead of Rume middleware pipelines, physical connection pooling across SQLite and PostgreSQL, competing outbox relay throughput under high concurrency via `SKIP LOCKED`, and durable worker task scheduling with 0.0% error rate SLA enforcement. | **Completed** |
| **REL-09** | CLI & Scaffolding Template Hardening | Update `rume new` templates to scaffold repository layering with first-class `rume::sql_assets!`, compile-time conventional migrations via `rume::embed_migrations!`, and core persistence schema chaining via `.with_rume_core()`. | **Completed** |
| **REL-10** | Phase 7.2 Runtime Contract Closure & Deployment Qualification | Fix `TestServer` lifecycle to hold application modules and database pools active until TCP connections drain; verify multi-instance `JobWorker` runtime across competing workers with heartbeats and typed handlers; verify subprocess leader takeover via `RedisLeaderElector` with monotonic terms and stale renewal rejection; enforce strict zero-error SLA budgets and environment metadata recording; qualify standalone reference application scenarios (`workbench/simple-api` and `workbench/modular-monolith`); and add Docker Compose multi-instance system test qualification (`workbench/multi-instance`) validating clean deployment, concurrent startup migration locking via `pg_advisory_xact_lock`, two-instance state consistency, cross-instance transactional outbox & worker execution, instance SIGKILL failure, restart recovery, Redis/Postgres dependency restarts, and SIGTERM graceful shutdown drain. | **Completed** |
| **REL-11** | Production Deployment Guides & Observability | Comprehensive documentation for Kubernetes deployment, health probes, Prometheus metrics scraping, OpenTelemetry tracing export (bring-your-own-subscriber model), and zero-downtime rolling upgrades. | **P1** |
| **REL-12** | Crate Publishing & Semantic Versioning Audit | Final audit of crate dependencies, API surface documentation, `#![forbid(unsafe_code)]` verification across all workspace crates, and v0.1.0 release packaging. | **P1** |

**Verification Suites:**
- [`workbench/multi-instance/benches/baseline.rs`](../../../workbench/multi-instance/benches/baseline.rs) (Real TCP network benchmark across concurrency levels [1, 8, 32, 64, 128], physical connection checkout & query execution, competing outbox relay throughput, and durable worker task scheduling).
- [`workbench/multi-instance/tests/performance_baseline.rs`](../../../workbench/multi-instance/tests/performance_baseline.rs) (Automated performance qualification and hard SLA budget assertion with 0.0% error rate).
- [`workbench/multi-instance/tests/compose_system_test.rs`](../../../workbench/multi-instance/tests/compose_system_test.rs) (Layer 3 Docker Compose system test suite validating independent container deployment, concurrent startup migration serialization via `pg_advisory_xact_lock`, cross-process outbox/job processing, SIGKILL crash failover, restart recovery, dependency recovery, and SIGTERM graceful drain).
- [`crates/core/tests/containers/crash_recovery.rs`](../../../crates/core/tests/containers/crash_recovery.rs) (Subprocess crash recovery harness exercising `SIGKILL` / `child.kill()` failure injection during worker execution, business transaction outbox commit, and distributed leader takeover).
- [`crates/core/tests/integration/test_server.rs`](../../../crates/core/tests/integration/test_server.rs) (TCP `TestServer` suite verifying connection keep-alive, chunked streaming, 2MB body handling, raw client drop disconnects, and graceful shutdown with active requests).
- [`crates/cli/tests/scaffold_template.rs`](../../../crates/cli/tests/scaffold_template.rs) (Scaffolding template verification with SQL assets, repository patterns, and embed migrations).
- Reference Application Scenarios:
  - [`workbench/simple-api`](../../../workbench/simple-api) (Minimal single-binary REST API with routing, validation, config, and SQLite persistence).
  - [`workbench/modular-monolith`](../../../workbench/modular-monolith) (Modular monolith with `catalog` and `orders` domain modules, inter-module events, and topological lifecycle startup/shutdown).
  - [`workbench/multi-instance`](../../../workbench/multi-instance) (Full multi-tenant issue-tracking enterprise SaaS application with PostgreSQL transactions, transactional outbox, durable jobs, and Redis deduplication).

---

Every capability on this roadmap is evaluated across five dimensions:

1. **Simplicity:** Easier to build everyday features than the alternatives. Zero ceremonial overhead for small applications.
2. **Maintainability:** Architecture remains understandable and modular as applications grow.
3. **Extensibility:** New capabilities and domain modules compose cleanly without fighting framework abstractions.
4. **Flexibility:** Developers retain sovereign control of their persistence, authentication, and external services.
5. **Reliability:** Predictable runtime semantics, strict type guarantees, and fail-closed security.
