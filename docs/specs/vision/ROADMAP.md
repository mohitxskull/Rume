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

**Verification Suite:** [`rume/tests/progressive_composition.rs`](../../../rume/tests/progressive_composition.rs) (8/8 integration tests passing).

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
- [`rume/tests/validation_suite.rs`](../../../rume/tests/validation_suite.rs)
- [`rume/tests/persistence_integration.rs`](../../../rume/tests/persistence_integration.rs)
- [`rume/tests/auth_suite.rs`](../../../rume/tests/auth_suite.rs)
- [`rume/tests/cache_suite.rs`](../../../rume/tests/cache_suite.rs)
- [`rume/tests/jobs_suite.rs`](../../../rume/tests/jobs_suite.rs)
- [`rume/tests/events_suite.rs`](../../../rume/tests/events_suite.rs)
- [`rume/tests/openapi_suite.rs`](../../../rume/tests/openapi_suite.rs)

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
| **ENT-04** | Unified Observability | Correlated OpenTelemetry distributed tracing, Prometheus metrics export, structured JSON logging, and error tracking. | **Completed** |
| **ENT-05** | Module Integration Testing | In-memory and test-container test harnesses allowing individual modules to be tested with real dependencies or explicit test doubles. | **Completed** |
| **ENT-06** | Architecture CI Verification | Compile-time and CI test runners enforcing module visibility, forbidden cross-domain calls, and clean dependency graphs. | **Completed** |
| **ENT-07** | Multi-Tenant Primitives | Optional tenant resolution extractors, connection scoping, and tenant-isolated cache/storage boundaries. | **Completed** |
| **ENT-08** | Security Hardening | Hardened security headers, CSRF protection, constant-time comparisons, and audit log integration. | **Completed** |

**Verification Suites:**
- [`rume/tests/outbox_suite.rs`](../../../rume/tests/outbox_suite.rs)
- [`rume/tests/job_reliability_suite.rs`](../../../rume/tests/job_reliability_suite.rs)
- [`rume/tests/resilience_suite.rs`](../../../rume/tests/resilience_suite.rs)
- [`rume/tests/metrics_suite.rs`](../../../rume/tests/metrics_suite.rs)
- [`rume/tests/module_testing_and_architecture_suite.rs`](../../../rume/tests/module_testing_and_architecture_suite.rs)
- [`rume/tests/tenant_suite.rs`](../../../rume/tests/tenant_suite.rs)
- [`rume/tests/security_suite.rs`](../../../rume/tests/security_suite.rs)

---

## Phase 4 — Framework Maturation

> **Priority:** P2 · Long-Term Expansion  
> **Status:** Completed & Verified

Specialized enterprise and distributed extensions built on the proven core architecture and capability layers:

| ID | Task | Acceptance Criteria | Target |
|---|---|---|---|
| **MAT-01** | Workflow Orchestration | Multi-step saga execution with functional or trait steps, automated reverse compensation hooks, and full execution tracing. | **Completed** |
| **MAT-02** | Distributed Coordination | Exclusive distributed locking with monotonic fencing tokens and TTL auto-expiration; leader election with lease renewal. | **Completed** |
| **MAT-03** | Enterprise Protocol Starters | Standard SCIM 2.0 (RFC 7643 / RFC 7644) provisioning envelopes and SAML 2.0 assertion parser, audience/condition validator. | **Completed** |

**Verification Suites:**
- [`rume/tests/saga_suite.rs`](../../../rume/tests/saga_suite.rs)
- [`rume/tests/coordination_suite.rs`](../../../rume/tests/coordination_suite.rs)
- [`rume/tests/enterprise_suite.rs`](../../../rume/tests/enterprise_suite.rs)

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
| **REL-06** | Security & Enterprise Hardening | Standards-compliant XML parsing for SAML 2.0 with mandatory digital signature verification (protecting against XML signature-wrapping attacks), audience/issuer/time-condition validation, and hardened tenant isolation. | **P1** |
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

**Verification Suites:**
- [`workbench/tests/workbench_test.rs`](../../../workbench/tests/workbench_test.rs) (End-to-end multi-tenant CRUD, auth/authz, transaction-enlisted outbox, worker processing, and graceful drain).
- [`workbench/tests/deployment_validation.rs`](../../../workbench/tests/deployment_validation.rs) (Multi-instance concurrency across tenants, Redis idempotency deduplication, competing outbox relays, and worker shutdown supervision).

---

Every capability on this roadmap is evaluated across five dimensions:

1. **Simplicity:** Easier to build everyday features than the alternatives. Zero ceremonial overhead for small applications.
2. **Maintainability:** Architecture remains understandable and modular as applications grow.
3. **Extensibility:** New capabilities and domain modules compose cleanly without fighting framework abstractions.
4. **Flexibility:** Developers retain sovereign control of their persistence, authentication, and external services.
5. **Reliability:** Predictable runtime semantics, strict type guarantees, and fail-closed security.
