# Rume — Vision

> **Status: pre-v1 draft.** The application foundation for developers who choose Axum but do not want to repeatedly assemble and maintain the same application infrastructure themselves.

---

## 1. Executive Summary

Rume makes building and operating an Axum application predictable.

It provides a coherent application structure, typed configuration and environment loading, explicit routing, request validation, consistent error handling, lifecycle management, observability, testing utilities, and developer tooling.

Rume handles the repetitive framework-level work so developers can concentrate on application behavior.

It is built on Axum and Tower, respects Rust's type system, and keeps application architecture visible in ordinary Rust code.

### The Strategic Identity of Rume

> **"The application foundation for developers who choose Axum but do not want to repeatedly assemble and maintain the same application infrastructure themselves."**

This positioning gives every future feature a clear litmus test:
*Does this make an Axum application more coherent, more predictable, or easier to operate without hiding how it works?*
If not, it does not belong in Rume's core.

---

## 2. The Problem Rume Solves

Building a Rust HTTP application is straightforward. Building a maintainable application with reliable configuration, validation, error contracts, operational endpoints, request observability, test infrastructure, and a repeatable project structure requires assembling many independent pieces:

1. Teams repeatedly assemble configuration, environment loading, structured logging, request tracing, authentication, parameter validation, database pools, error handling, health checks, testing harnesses, and deployment readiness by hand.
2. Teams repeatedly solve the exact same integration problems across every new project.
3. Teams discover the operational gaps (secret leakage in logs, unvalidated environments, lifecycle race conditions on deployment) late in production.

Rume provides a tested, coherent foundation for those problems without forcing teams into an opinionated database, ORM, or application architecture.

---

## 3. Target Users

Small engineering teams (1–8 developers) building APIs and SaaS applications with Rust.

They want:
- Axum's performance and flexibility paired with a structured, predictable application foundation.
- Typed configuration and boundary environment validation.
- Explicit application state and dependency composition (no hidden DI containers).
- Predictable routing, validation, and RFC 9457 error handling.
- Built-in operational lifecycle behaviors (liveness, readiness, graceful drain).
- Productive CLI generators and project tooling.
- Complete freedom to select their own database, authentication system, message broker, and deployment infrastructure.

---

## 4. What Rume Provides: Six Core Capabilities

Rume's core product is defined by six cohesive capabilities. Anything outside these capabilities must justify why it belongs in the core crate.

| Capability | Responsibility |
|---|---|
| **Application Foundation** | App context, explicit state, project paths, builder pattern, lifecycle state machine, and ordered startup/shutdown hooks. |
| **Configuration** | Layered environment (`.env`) and TOML configuration loading, typed deserialization, provenance tracking, and leak-proof secret redaction. |
| **HTTP Foundation** | Explicit routing, boundary parameter validation, onion-model Tower middleware, RFC 9457 Problem Details error contracts, and request identity. |
| **Operations** | Liveness probes, readiness checks, health check registry, graceful shutdown with in-flight request drainage, and request tracing observability. |
| **Developer Experience** | First-class CLI (`rume`), project scaffolding, code generators, diagnostic tooling, and single-source-of-truth route inspection. |
| **Testing** | In-memory HTTP test client (`TestApp`), test application builder, lifecycle test utilities, and integration testing harnesses. |

---

## 5. Design Principles

These design principles are non-negotiable:

1. **Explicit over implicit.** Dependencies, state, routes, and middleware are constructed visibly in plain Rust code. No classpath scanning, no magic macros, no runtime reflection.
2. **Typed over stringly typed.** Configuration, requests, responses, route parameters, and errors use Rust types validated at the boundary. No stringly-typed `config.get::<T>("some.key")` in application code.
3. **One source of truth.** A route, configuration declaration, or generated component must never require duplicate manual registration. Declaring a route automatically mounts it and registers its inspectable metadata.
4. **Correctness over convenience.** Framework abstractions must preserve Axum and Tower semantics and fail closed predictably rather than silently degrade.
5. **Operational behavior by design.** Health probes, readiness state transitions, graceful shutdown drainage, request identity propagation, and structured tracing are part of the baseline foundation, not afterthoughts.
6. **Axum-native, not Axum-replacing.** Rume builds cleanly on top of Axum, Tower, and Tokio rather than inventing alternative HTTP engines or competing abstractions.
7. **Small core, modular extensions.** The core crate remains lean and cohesive. Integrations for specific databases, message brokers, or external auth providers belong in independent extension crates.

---

## 6. What Rume Deliberately Does NOT Provide

Rume maintains strict architectural boundaries:

- **No runtime dependency-injection container:** Rust's type system and ownership model make explicit struct construction clearer, faster, and compile-time verified.
- **No classpath or module scanning:** Application composition is explicitly declared in code.
- **No mandatory ORM or database:** Teams choose SQLx, SeaORM, Diesel, or raw drivers as they see fit.
- **No hidden transaction boundaries:** Database transactions remain visible in application service code.
- **No mandatory authentication implementation:** Applications implement session, JWT, API key, or OAuth flows appropriate for their domain.
- **No mandatory message queue, mailer, or storage provider:** Kept out of the core to avoid bloat.
- **No runtime plugin system:** Extension happens via standard Rust traits, Tower layers, and crates.
- **No attempt to reproduce Rails or Loco feature-for-feature:** Rume focuses on explicit correctness and operational reliability rather than monolithic conventions.

---

## 7. Verifiable Guarantees (Defining "Production-Ready")

Rather than making vague claims of production readiness, Rume defines readiness through observable, testable guarantees:

* **Startup Gate:** Invalid environment variables or configuration errors prevent application boot and fail closed before any network socket is bound or startup side-effects occur.
* **Secret Privacy:** Secret-valued variables, credential URLs, and sensitive field diagnostics are permanently redacted (`[redacted]`) and never leak into logs, error messages, or CLI output.
* **Fail-Closed Route Validation:** Route parameter extraction failures and malformed percent-encoded paths immediately produce sanitized 400 Problem Details responses and never invoke the handler.
* **Readiness-Before-Drain Shutdown:** Upon receiving a shutdown signal, availability immediately transitions to `RefusingTraffic` (returning 503 to health probes) *before* in-flight HTTP connections begin draining.
* **Traceable Request Identity:** Every request receives a valid, OS CSPRNG-backed UUID v4 `X-Request-ID` (unless a safe client ID is provided), which propagates across async yield points, tracing spans, and RFC 9457 error responses.
* **Portable Scaffold Quality:** Projects generated by `rume new` format, compile, and pass their unit and integration tests out of the box without requiring access to the framework's development checkout.
* **Canonical Route Inventory:** Tooling (`rume list:routes`) reports the actual routes declared and mounted by the application from a shared runtime registry, never from source-code heuristic regexes.

---

## 8. The Developer Journey as the Primary Product

Rume's primary product is a cohesive, end-to-end developer workflow:

```
  1. Create       2. Configure      3. Generate       4. Compose        5. Run & Test    6. Deploy
┌───────────┐    ┌─────────────┐   ┌─────────────┐   ┌─────────────┐   ┌─────────────┐  ┌─────────────┐
│ rume new  │───>│ Typed Env & │──>│  Handlers,  │──>│ Explicit    │──>│ Consistent  │─>│ Graceful    │
│  my-api   │    │ TOML Config │   │ Middleware, │   │ Routes &    │   │ Errors,     │  │ Readiness,  │
│           │    │ Validation  │   │ Resources   │   │ Lifecycle   │   │ Observability│ │ Health, Draining
└───────────┘    └─────────────┘   └─────────────┘   └─────────────┘   └─────────────┘  └─────────────┘
```

Every core feature must improve one or more steps in this journey.

---

## 9. The CLI as Core Framework Infrastructure

The CLI (`rume`) is an integral component of the framework, held to the same standards of correctness as the `rume` library:

- **Shared Metadata Model:** Route listing inspects the actual application route registry. It does not parse Rust source text with regular expressions.
- **Portable Scaffolding:** `rume new` produces portable, stand-alone Rust projects with configurable dependency resolution.
- **Reliable Generation:** Code generators for handlers, middleware, resources, services, and models handle duplicate names, invalid syntax, missing markers, and interrupted writes safely.

---

## 10. Stability Boundaries

| Category | Stability Expectation |
|---|---|
| **Core Public API** | Deliberately designed, thoroughly documented, and covered by integration tests. Breaking changes follow semantic versioning. |
| **CLI & Generators** | Reliable developer experience, predictable exit codes, and backwards-compatible project output where practical. |
| **Experimental Extensions** | Clearly demarcated; free to evolve rapidly based on real-world adoption before core stabilization. |

---

## 11. Performance and Complexity Constraints

Rume is an ultra-thin, high-performance layer over Axum and Tower:

- **Zero unnecessary runtime indirection:** Abstractions compile down to direct Tower service calls.
- **Zero hidden allocations in hot paths:** Request routing, parameter matching, and helper functions avoid heap allocations whenever possible (using zero-copy `Cow<'_, str>` and stack buffers).
- **No framework-owned background threads:** Background tasks, workers, or timers are only spawned when explicitly requested by application code.
- **Pure dependencies:** Every dependency in `Cargo.toml` must have an explicit, justified purpose. `#![forbid(unsafe_code)]` remains strictly enforced.
- **Compile-time and binary-size tracking:** Framework overhead is measured against an equivalent vanilla Axum baseline.

---

## 12. Success Metrics & Quality Rule

| Metric | Target |
|---|---|
| Time from `rume new` to first running endpoint | Under 10 minutes |
| Time from `rume new` to first deployed endpoint | Under 30 minutes |
| Lines of configuration for a basic API | Under 20 meaningful lines |
| Concepts to learn before creating the first route | Under 5 |
| Generated project compilation | 100% clean build, formats, and passes tests |
| Generator output validity | All generated components compile and register cleanly |
| Route inventory accuracy | Matches 100% of routes declared through Rume APIs |
| Diagnostic safety | 0 secret values or credentials leaked in errors/logs |
| Shutdown predictability | Availability transitions to `RefusingTraffic` before drain |
| Runtime overhead | Within 5% of raw Axum baseline |

> **The Quality Rule:**
> *No feature is considered complete because its API exists. It is complete when its intended developer workflow works end to end with integration test verification.*

---

## 13. Implementation Roadmap

### Milestone 1 — Make the Framework Coherent (Highest Priority / P0)
*Goal: Eliminate inconsistencies between public APIs, routing, and developer inspection.*

- **R1. Canonical Route Registration:** Create a unified route declaration and metadata model where mounting a route and recording its metadata are one atomic operation.
- **R2. Route Inspection Redesign:** Replace source-code heuristic parsing in `rume list:routes` with programmatic inspection of the application's runtime route registry.
- **R3. Route Metadata Integration Tests:** Comprehensive test suite asserting that HTTP methods, names, route parameters, and paths accurately match mounted handlers across groups and resources.
- **R4. Axum Interoperability Contract:** Explicitly document and test router composition (plain Axum routes, nested sub-routers, merged routers, and custom layers).

### Milestone 2 — Make the CLI Distributable and Trustworthy (Release Blocker / P0)
*Goal: Ensure developers can install the CLI independently and scaffold portable, robust projects.*

- **C1. Portable Dependency Resolution:** Update `rume new` templates so generated projects depend on published/configurable crate versions rather than requiring a local relative path.
- **C2. Local Development Override:** Provide an explicit workflow for contributors testing local framework changes against generated projects.
- **C3. Generator Test Coverage:** Complete integration test suite covering handler, resource, middleware, model, and service generators.
- **C4. Generator Safety:** Ensure idempotent, safe file modifications with rollback on interrupted writes and clear conflict detection.
- **C5. CLI Command Contracts:** Standardize exit codes, error reporting, and machine-readable JSON output across all CLI commands.
- **C6. Clean-Install Integration Test:** Automated CI test that installs the CLI binary in an isolated environment, scaffolds a new project, compiles it, and executes all smoke tests without repository checkout.

### Milestone 3 — Make Rume Easy to Adopt (Developer Experience / P1)
*Goal: Ensure developers can build production applications without reading framework source code.*

- **D1. Getting-Started Guide:** Step-by-step walkthrough creating, configuring, generating, running, and testing an API.
- **D2. Architecture Guide:** Clear explanations of context, state, paths, routing, middleware onion model, and lifecycle states.
- **D3. Configuration & Secrets Guide:** Complete reference covering precedence, interpolation, secret redaction, and typed validation.
- **D4. Routing Guide:** Patterns for validated parameters, route groups, resources, and custom matchers.
- **D5. Operations Guide:** Deployment reference for health probes, liveness/readiness, graceful shutdown, request tracing, and problem details.
- **D6. Generator Reference:** Documentation of all CLI generator commands and their customization conventions.
- **D7. Reference Application:** A complete, realistic reference service (users and posts API) demonstrating all six core capabilities end-to-end.

### Milestone 4 — Establish v0.1 Release Quality (Release Preparation / P2)
*Goal: Publish a rock-solid v0.1 release with verified guarantees.*

- **Q1. Public API Audit:** Review public exports, prune internal utilities, and document all public items.
- **Q2. Dependency Audit:** Audit all workspace dependencies, document their rationale, and verify licenses.
- **Q3. Performance Baseline:** Benchmark startup time, memory footprint, request latency, and throughput against vanilla Axum.
- **Q4. Platform Verification:** Verify graceful shutdown and signal behavior across Linux, macOS, and containerized environments.
- **Q5. Documentation Tests:** Ensure all doc-tests compile and pass continuously.
- **Q6. Release Automation:** Automated CI workflows for versioning, changelog verification, packaging, and cargo publication.
- **Q7. Adoption Walkthrough:** Fresh developer validation verifying the complete getting-started journey without manual intervention.

---

## 14. What is Deliberately Postponed

To keep the core foundation cohesive and dependable, the following items are deliberately kept out of the core roadmap:

| Feature | Strategic Decision |
|---|---|
| Built-in ORM | Keep out of core; encourage SQLx/SeaORM |
| Authentication Framework | Separate integration crate |
| Background Job Engine | Post-v1 consideration |
| Mail & Notification System | Separate integration crate |
| Object / File Storage | Separate integration crate |
| WebSocket Abstraction | Add only when a concrete core use case emerges |
| Hot Reload | Handled by external ecosystem tools (`cargo-watch`, `bacon`) |
| Runtime DI & Plugin System | Permanently rejected |

---

## 15. Execution Sequence

```
1. Route Architecture (P0)     ──> Canonical declaration, registry & inspection
2. Portable CLI (P0)           ──> Independent distribution, clean-install testing
3. Generator Reliability (P1)  ──> Full generator coverage (models, services, handlers)
4. Reference App & Guides (P1) ──> Proving the end-to-end developer experience
5. Release Engineering (P2)    ──> Benchmarks, API freeze, packaging & v0.1 release
```
