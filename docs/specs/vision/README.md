# Rume — Vision

> **Rume is a complete, modular Rust application framework that makes simple applications effortless and complex applications understandable. It provides strong conventions and integrated capabilities by default, while preserving explicit control, architectural freedom, and Rust's compile-time guarantees.**

---

## 1. Executive Summary

Rume redefines what a Rust web framework should feel like.

Rather than being an ultra-thin layer that leaves teams to repeatedly assemble foundational infrastructure or an opaque monolithic framework that hides execution behind magic runtime containers, Rume provides **progressive complexity**:

* **Start simple:** A single-file, stateless application starts with three lines of code and zero framework ceremony.
* **Scale by design:** As complexity grows, developers introduce services, modular domains, persistence, and explicit boundaries when justified.
* **Architectural integrity:** Enterprise applications compose isolated, independently testable domains (`UsersModule`, `BillingModule`) with compile-time state encapsulation, cycle detection, and topological lifecycle orchestration without fighting framework abstractions.

Axum serves as Rume's rock-solid HTTP foundation, not the limit of what Rume becomes.

---

## 2. What Rume Learns from Established Frameworks

Rume studies the strengths and failure modes of Rails, Spring Boot, and AdonisJS:

### Ruby on Rails — Developer Experience & Conventions
* **Adopt:** Strong default project structure, generators that create complete, connected components, conventional resource routing, and modular packaging (engines).
* **Avoid:** Hidden business logic in model callbacks, implicit persistence side effects, god-object models, and conventions that resist override.
* **Rume Core Law:** *Conventions must reduce decisions, not conceal execution.*

### Spring Boot & Spring Modulith — Architectural Discipline & Enterprise Modularity
* **Adopt:** Architectural modularity with explicit boundaries, dependency graph verification (preventing circular module dependencies and illegal access to module internals), comprehensive operational contracts, and robust lifecycle hooks.
* **Avoid:** Opaque runtime application contexts, reflection-based dependency discovery, excessive runtime indirection, and auto-configuration that silently alters runtime behavior.
* **Rume Core Law:** *Enterprise readiness requires managing complexity across modules, not adding runtime magic.*

### AdonisJS — Cohesion & Ergonomic System Design
* **Adopt:** A coherent developer experience where CLI, configuration, validation, lifecycle, and testing feel designed by one team with unified semantics.
* **Avoid:** Building the framework upon a runtime IoC container, global singleton services that obscure dependencies, or forcing an all-or-nothing monolith.
* **Rume Core Law:** *Modules must integrate naturally, but application dependencies must remain visible.*

---

## 3. Core Architectural Principles

```
                 Start Simple. Scale by Design.
  ┌─────────────────────────────────────────────────────────────┐
  │ 1. Convention-led                                           │
  │    Sensible defaults, predictable structure, generators.    │
  ├─────────────────────────────────────────────────────────────┤
  │ 2. Explicit by construction                                 │
  │    Visible dependencies, transactions, and composition.     │
  ├─────────────────────────────────────────────────────────────┤
  │ 3. Modular by architecture                                  │
  │    Applications composed of bounded functional modules.     │
  ├─────────────────────────────────────────────────────────────┤
  │ 4. Progressive complexity                                   │
  │    Zero ceremony for small apps; deep structure when needed.│
  └─────────────────────────────────────────────────────────────┘
```

1. **Convention-led:** Sensible defaults, predictable directory layouts, automated scaffolding, and consistent APIs minimize decision fatigue for everyday features.
2. **Explicit by construction:** Dependencies, database transactions, background execution, and side effects remain visible in plain Rust code. No hidden classpath scanning, no magic runtime dependency-injection containers.
3. **Modular by architecture:** An application is not an undifferentiated pile of folders; it is composed of bounded, functional domains that encapsulate private state, expose explicit contracts, and enforce architectural rules.
4. **Progressive complexity:** Advanced capabilities reveal themselves only when the application demands them. A microservice or prototype does not pay the cognitive tax of enterprise architecture.

---

## 4. The Three Framework Layers

Rume is organized into three architectural layers:

```
┌─────────────────────────────────────────────────────────────────┐
│                    3. Application Modules                       │
│     (UsersModule, BillingModule, OrdersModule, Notifications)   │
├─────────────────────────────────────────────────────────────────┤
│                    2. Integrated Capabilities                   │
│  HTTP (Axum)  │  Persistence  │  Auth & Identity │  Background  │
│  Validation   │  Migrations   │  Authorization   │  Events/Jobs │
├─────────────────────────────────────────────────────────────────┤
│                       1. Rume Kernel                            │
│  Application Composition │ Module Lifecycle │ Typed Config      │
│  Security Redaction      │ RFC 9457 Errors  │ Test Harness      │
└─────────────────────────────────────────────────────────────────┘
```

1. **Rume Kernel:** Progressive application composition, first-class module registry, topological lifecycle orchestration, typed environment and configuration loading, secret privacy, error contracts, request identity, and testing infrastructure.
2. **Integrated Capabilities (Optional & Cohesive):** 
   - **HTTP:** Axum routing, Tower middleware onion, parameter validation, and OpenAPI inspection.
   - **Persistence:** Lifecycle-managed connection pools, migrations, and visible transaction boundaries.
   - **Identity & Auth:** Pluggable authentication providers, session management, and explicit policy authorization.
   - **Execution & Events:** Background workers, job queues, transactional outbox, and durable domain events.
   - **Infrastructure & Ops:** Health check registry, graceful connection drain, cache adapters, mail/storage interfaces, and distributed tracing.
3. **Application Modules:** Independent business domains that encapsulate private state, declare explicit dependencies, and register owned routes, middleware, and diagnostics.

---

## 5. First-Class Application Modules

In Rume, application modules (`Module`) are primary architectural building blocks.

### Progressive Scale in Practice

```rust
// Level 1: Minimal Stateless Application
#[tokio::main]
async fn main() -> Result<(), std::io::Error> {
    Rume::new()
        .get("/", || async { "Hello, Rume!" })
        .run()
        .await
}
```

```rust
// Level 2: Modular Enterprise Application
#[tokio::main]
async fn main() -> Result<(), std::io::Error> {
    Rume::new()
        .module(UsersModule::new(users_state))
        .module(BillingModule::new(billing_state))
        .module(OrdersModule::new(orders_state))
        .run()
        .await
}
```

### Module Responsibilities & Capabilities

| Capability | Scope |
|---|---|
| **Encapsulated State** | Owns domain-specific state (e.g. `UsersState`) scoped via `.with_state(my_state)`, eliminating global state leakage. |
| **Owned Routes** | Mounts HTTP endpoints and resources with metadata registration. |
| **Dependencies** | Declares explicit dependencies on other modules (e.g. `BillingModule` depends on `UsersModule`). |
| **Preflight Verification** | Cycle detection (DFS) and missing dependency detection fail closed during preflight before socket bind or startup side effects. |
| **Topological Lifecycle** | Initializes in topological dependency order (`on_start`); shuts down and rolls back in reverse topological order (`on_shutdown`). |
| **Owned Health Checks** | Registers domain diagnostics directly into the central health registry. |
| **Scoped Middleware** | Applies Tower layers and middleware specific to the module boundary. |

---

## 6. Engineering Rules & Anti-Patterns Guarded Against

| Anti-Pattern Guarded Against | Rume's Engineering Rule |
|---|---|
| **Fat Handlers** | HTTP handlers remain strictly focused on transport decoding, validation, and domain orchestration. |
| **Fat Models & Hidden Callbacks** | Business logic belongs in explicit domain services, never in opaque database lifecycle hooks. |
| **Global Service Locators / Magic DI** | Dependencies are constructed visibly and passed via typed Rust parameters. |
| **Hidden Transaction Boundaries** | Database transactions must be explicit and visible in service orchestration code. |
| **Uncontrolled Module Coupling** | Modules interact only through exposed public interfaces; internal state and repositories stay private. |
| **Fire-and-Forget Message Loss** | Async domain events require durable publication (transactional outbox) for reliable delivery. |
| **One Giant Config Monolith** | Modules own domain configuration settings while sharing framework validation rules. |
| **Inviolable Black Boxes** | Always preserve direct Axum, Tower, and Tokio interoperability. |

---

## 7. The Standard: Five Dimensions of Excellence

Rume evaluates every feature and architectural decision across five dimensions:

| Dimension | Definition of Success |
|---|---|
| **Simplicity** | Faster to build everyday features and onboard new developers than the alternatives. Minimal ceremony for small apps. |
| **Maintainability** | Architecture remains understandable, verifiable, and modular as the codebase scales from 1,000 to 1,000,000 lines. |
| **Extensibility** | New capabilities, custom Tower middleware, and domain modules compose cleanly without fighting framework abstractions. |
| **Flexibility** | Developers retain complete sovereignty over their database, authentication model, external services, and deployment targets. |
| **Reliability** | Zero secret leakage, fail-closed preflight, OS CSPRNG request identity, deterministic graceful drain, and compile-time Rust guarantees. |

---

## 8. Verifiable Guarantees (Production Readiness)

Rume defines production readiness through strict, verifiable guarantees:

* **Preflight Gate:** Invalid configuration, missing environment variables, circular module dependencies, or missing dependencies abort startup with zero side effects before any socket is bound.
* **Permanent Secret Privacy:** Passwords, API tokens, credential URLs, and sensitive error diagnostics are permanently redacted (`[redacted]`) and never leak into logs, errors, or CLI outputs.
* **Fail-Closed Route Validation:** Parameter validation rejections immediately produce sanitized RFC 9457 Problem Details responses and never invoke the handler.
* **Readiness-Before-Drain Graceful Shutdown:** Upon shutdown signal, availability immediately transitions to `RefusingTraffic` (returning 503 to load balancers) *before* in-flight HTTP connections begin draining.
* **Traceable Request Identity:** Every request carries an OS CSPRNG-backed UUID v4 (RFC 9562) that propagates across async tasks, tracing spans, and RFC 9457 responses.
* **Portable Scaffolding:** CLI-generated projects format cleanly, compile cleanly, and pass all tests out of the box without local framework path dependencies.
* **Canonical Runtime Inventory:** Route listing reflects actual mounted runtime handlers and specs, never heuristic source-code regexes.

---

## 9. Engineering Roadmap

The phased engineering execution plan for this vision is detailed in **[`ROADMAP.md`](ROADMAP.md)**:
* **Phase 1: Progressive Application Architecture** (`ARCH-01` through `ARCH-07`) — *Completed & verified*
* **Phase 2: Complete Application Capabilities** (`CAP-01` through `CAP-08`) — *Next phase*
* **Phase 3: Enterprise Reliability** (`ENT-01` through `ENT-08`) — *Planned*
* **Phase 4: Framework Maturation** — *Future consideration*
