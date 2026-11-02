# Rume — Engineering Roadmap

> **This roadmap outlines the phased architectural evolution of Rume from an ergonomic application layer into a complete, modular Rust application framework built around progressive complexity.**

---

## Strategic Overview

Rume follows four development phases:
1. **Phase 1: Progressive Application Architecture** — Establish core composition, first-class modules, cycle detection, and topological lifecycle orchestration.
2. **Phase 2: Complete Application Capabilities** — Add first-party persistence, authentication, authorization, background execution, and application events.
3. **Phase 3: Enterprise Reliability** — Implement transactional outbox, job reliability, resilience middleware, unified observability, and architecture test verification.
4. **Phase 4: Framework Maturation** — Expand into advanced distributed processing, workflow orchestration, and enterprise ecosystem integrations.

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
> **Status:** Next Phase

This phase provides enterprise-grade reliability, data consistency, and architectural enforcement.

| ID | Task | Acceptance Criteria | Target |
|---|---|---|---|
| **ENT-01** | Transactional Outbox | Durable event publication pattern ensuring atomic database mutation and event emission without message loss on process crash. | Planned |
| **ENT-02** | Job Reliability | Distributed worker execution, idempotency keys, exponential backoff retries, dead-letter queues, and worker heartbeat recovery. | Planned |
| **ENT-03** | Resilience Middleware | Circuit breakers, adaptive timeouts, concurrency limits, rate limiting, and shed-load policies. | Planned |
| **ENT-04** | Unified Observability | Correlated OpenTelemetry distributed tracing, Prometheus metrics export, structured JSON logging, and error tracking. | Planned |
| **ENT-05** | Module Integration Testing | In-memory and test-container test harnesses allowing individual modules to be tested with real dependencies or explicit test doubles. | Planned |
| **ENT-06** | Architecture CI Verification | Compile-time and CI test runners enforcing module visibility, forbidden cross-domain calls, and clean dependency graphs. | Planned |
| **ENT-07** | Multi-Tenant Primitives | Optional tenant resolution extractors, connection scoping, and tenant-isolated cache/storage boundaries. | Planned |
| **ENT-08** | Security Hardening | Hardened security headers, CSRF protection, constant-time comparisons, and audit log integration. | Planned |

---

## Phase 4 — Framework Maturation

> **Priority:** P2 · Long-Term Expansion  
> **Status:** Future Consideration

Specialized extensions to be explored after the core architecture and capability layers have proven themselves in production code:

* **Workflow Orchestration:** Multi-step saga execution with compensation hooks.
* **Alternative Runtimes:** Support for alternative asynchronous runtimes and WASM execution targets.
* **Distributed Coordination:** Leader election and distributed lock interfaces.
* **Advanced Enterprise Starters:** Pre-packaged modules for common compliance and enterprise protocols (SAML, SCIM, Audit Trails).

---

## Standards of Evaluation

Every capability on this roadmap is evaluated across five dimensions:

1. **Simplicity:** Easier to build everyday features than the alternatives. Zero ceremonial overhead for small applications.
2. **Maintainability:** Architecture remains understandable and modular as applications grow.
3. **Extensibility:** New capabilities and domain modules compose cleanly without fighting framework abstractions.
4. **Flexibility:** Developers retain sovereign control of their persistence, authentication, and external services.
5. **Reliability:** Predictable runtime semantics, strict type guarantees, and fail-closed security.
