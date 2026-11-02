# Architecture Review — v02

## Summary

Rume v02 successfully realizes its architectural vision of a progressive, zero-magic application framework for Axum. The core additions—a 3-line progressive entry point (`Rume::new()`), first-class domain modules (`Module`), post-order DFS circular dependency detection with cycle path reporting, topological lifecycle startup with reverse topological rollback on failure, and isolated module testing (`ModuleHarness`)—provide outstanding modularity without runtime reflection, classpath scanning, or hidden DI containers. Third-party crate leakage from v01 has been cleanly expunged from `lib.rs` and `prelude.rs`. However, three major architectural defects require remediation: `rume::prelude` has over-expanded to re-export niche enterprise protocols (SCIM, SAML), distributed coordination locks, and string utilities across all 24 subsystems, polluting consumer namespaces; `ArchitectureRules::verify` only inspects direct module dependencies, allowing transitive architecture violations to pass unnoticed in CI; and route collisions between modules or with application routes trigger unhandled runtime panics in Axum rather than failing closed through `BuildError`.

## Prior decisions

- **A-M1 (Public API surface leaks unrelated third-party crates; prelude too broad)** — **REAFFIRMED**. Verified in `rume/src/lib.rs:34–100` and `rume/src/prelude.rs:75–78`. Whole third-party crate re-exports (`pub use axum;`, `pub use tokio;`, `pub use serde;`, `pub use serde_json;`, `pub use tracing;`) have been completely removed from `lib.rs`. In `prelude.rs`, only standard extractors and derives (`Json`, `Path`, `State` from Axum, `Deserialize`, `Serialize` from Serde) remain. However, the internal breadth of `prelude.rs` has expanded with v02 subsystem types, which is evaluated under finding M1.
- **A-M2 (`Rume::builder` documented as sole entry point, but `RumeBuilder::new` is public)** — **REAFFIRMED**. Verified in `rume/src/app/builder.rs:295`. `RumeBuilder::new` was made `pub(crate)`, making `Rume::builder(ctx)` (for typed custom context) and `Rume::new()` (for progressive defaults) the only public entry points for constructing an application.
- **m1 (`init_tracing()` silently discards subscriber initialization failure)** — **CHALLENGED**. Verified in `rume/src/app/shutdown.rs:16–19`. The v01 review recommended returning a `Result` or logging the failure. In v02, `let _ = fmt().with_env_filter(filter).try_init();` was replaced with `fmt::fmt().with_env_filter(filter).init();`. Calling `.init()` panics if a global dispatcher is already registered (e.g. in multi-test harnesses, nested test runners, or embedding applications that initialize tracing prior to Rume). The recommendation to provide a fallible or non-panicking initialization path remains unresolved and is tracked under finding m4.
- **m2 (Lifecycle/Availability atomic state coupled to enum declaration order)** — **REAFFIRMED**. Verified in `rume/src/app/state.rs:47–54, 114–121`. Both `LifecycleState` and `AvailabilityState` now carry explicit discriminants and `#[repr(u8)]` pinning (`Created = 0`, `Booted = 1`, `Ready = 2`, `Terminating = 3`, `Terminated = 4` and `Unknown = 0`, `Live = 1`, `Ready = 2`, `RefusingTraffic = 3`, `Broken = 4`). In addition, `set()` methods now accept the typed enum directly and guard against out-of-range discriminants with `MAX_DISCRIMINANT`. Verified in unit tests `lifecycle_invalid_u8_maps_to_created` and `availability_invalid_u8_maps_to_unknown`.
- **NOTE-NEW (ARCH-01: First-Class Module System and Progressive Composition)** — Introduced in `rume/src/module/` and `rume/src/app/builder.rs:196–210, 319–334`. Provides `Rume::new()`, `Module` traits, scoped state encapsulation, DAG cycle detection, and topological startup/shutdown lifecycle sequencing. Evaluated throughout this report.
- **NOTE-NEW (ARCH-02: CI Architecture Enforcement via `ArchitectureRules`)** — Introduced in `rume/src/module/architecture.rs`. Provides static validation for required modules and forbidden cross-module dependencies in CI test suites. Evaluated in finding M2.

## Findings

### M1 Over-broad `prelude.rs` leaks niche enterprise schemas, coordination primitives, and string utilities

**Where:** `rume/src/prelude.rs` lines 8–74.

**What it says:**
`prelude.rs` re-exports 70+ types and functions across all 24 subsystems, including enterprise protocol schemas (`SCIM_USER_SCHEMA`, `ScimUser`, `ScimEmail`, `SamlAssertion`, `SamlParser`), distributed coordination primitives (`DistributedLock`, `LeaderElector`, `LockGuard`), saga orchestration (`Saga`, `Step`, `FunctionalStep`), background jobs (`Job`, `QueuedJob`, `RetryPolicy`), outbox patterns (`OutboxMessage`, `OutboxRelay`), metrics counters, tenant helpers, and raw string utilities (`slug`, `truncate`, `mask_email`, `safe_equal`).

**Why it's a problem:**
A prelude is intended for universal types and traits that every Axum handler or controller needs (`AppContext`, `Rume`, `Json`, `State`, `Path`, `Router`). Flooding `rume::prelude` with domain-specific types like `Step`, `Job`, `Event`, `Counter`, `Gauge`, `Session`, `Identity`, `Migration`, and `Tenant` causes severe namespace pollution and name collisions with users' own application domain types. When an application developer writes `use rume::prelude::*;`, declaring a `struct Job` or `struct Step` or `struct Tenant` creates an ambiguous import collision. Subsystem-specific types belong in their respective modules (`rume::enterprise::*`, `rume::saga::*`, `rume::coordination::*`), not the universal prelude.

**Suggested fix:**
Prune `rume::prelude` to core application essentials:
- Application bootstrap: `Rume`, `RumeBuilder`, `AppContext`, `Environment`, `Availability`, `Lifecycle`
- Routing & modularity: `Router`, `RouteGroup`, `RouteSpec`, `Module`, `ModuleContext`
- HTTP extractors & derives: `Json`, `Path`, `State`, `Deserialize`, `Serialize`
Remove `enterprise` (`SCIM_*`, `Saml*`), `saga`, `coordination`, `jobs`, `outbox`, `tenant`, and utility helpers from `prelude.rs`. Users who need those subsystems should import them from their dedicated namespaces (e.g. `use rume::jobs::Job;`, `use rume::enterprise::scim::*;`).

---

### M2 `ArchitectureRules::verify` only inspects direct dependencies, allowing indirect forbidden cross-domain dependencies

**Where:** `rume/src/module/architecture.rs` lines 76–90.

**What it says:**

```rust
        for module in modules {
            let from_name = module.name();
            let declared_deps = module.dependencies();

            for (forbidden_from, forbidden_to) in &self.forbidden_deps {
                if from_name == forbidden_from && declared_deps.contains(&forbidden_to.as_str()) {
                    return Err(ArchitectureViolation {
                        rule: "forbid_dependency".to_string(),
                        details: format!(
                            "module '{from_name}' is forbidden from depending on '{forbidden_to}', but declares it as a dependency"
                        ),
                    });
                }
            }
        }
```

**Why it's a problem:**
`verify` only inspects the immediate `declared_deps` slice of `from_module`. If an architecture rule specifies `.forbid_dependency("billing", "database_internal")`, and module `"billing"` depends on `"orders"` which in turn depends on `"database_internal"`, `declared_deps` for `"billing"` is `["orders"]`. The forbidden transitive dependency (`billing -> orders -> database_internal`) is completely ignored by `verify`, producing a false-clean pass in CI. In a modular monolith, enforcing architectural boundaries between domains requires checking the transitive closure of dependencies, not just immediate 1-hop edges.

**Suggested fix:**
Update `ArchitectureRules::verify` to compute the transitive dependency closure for each module (or perform a reachability traversal using the existing DFS algorithm from `ModuleRegistry`) to ensure that forbidden dependencies cannot be reached directly or indirectly through intermediate modules.

---

### M3 Route merge collisions between modules or application routes trigger unhandled runtime panics instead of failing preflight

**Where:** `rume/src/app/builder.rs` lines 788–791.

**What it says:**

```rust
        let (module_axum, module_routes) = module_router.into_parts();
        let adapted: Router<AppContext<E, C, S>> = module_axum.with_state(());
        let router = router.merge(adapted);
        route_registry.extend(module_routes);
```

**Why it's a problem:**
If two modules register the same HTTP method and route path, or if a module registers a route already mounted directly on `RumeBuilder`, `router.merge(adapted)` invokes Axum's underlying `Router::merge`, which panics with `"Cannot merge two routers that both define a route for: ..."`. Rume explicitly contracts fail-closed preflight returning `Result<..., BuildError>` (e.g. for middleware typos and module dependency cycles), yet route collisions cause an unhandled process panic rather than returning a typed `BuildError::DuplicateRoute`.

**Suggested fix:**
Before calling `router.merge(adapted)`, inspect `module_routes` for duplicate `(method, path)` pairs against each other and against `route_registry`. If a collision is detected, return `Err(BuildError::Module(ModuleError::DuplicateRoute { method, path }))` or a dedicated `BuildError::DuplicateRoute` variant so preflight fails closed cleanly with structured error reporting.

---

### m1 Orphaned `ops_router` duplicates `health_routes` with divergent paths and state models

**Where:** `rume/src/app/shutdown.rs` lines 58–88.

**What it says:**

```rust
pub fn ops_router(router: Router<Availability>) -> Router<Availability> {
    router
        .route(
            "/ops/live",
            get(move |State(av): State<Availability>| async move { ... }),
        )
        .route(
            "/ops/ready",
            get(move |State(av): State<Availability>| async move { ... }),
        )
}
```

**Why it's a problem:**
Rume maintains two disconnected health-route APIs: `health_routes()` in `routing/health.rs` (mounted on `Router<AppContext<E, C, S>>` at `/health/live` and `/health/ready`, running `HealthChecks`) and `ops_router()` in `app/shutdown.rs` (operating on `Router<Availability>` at `/ops/live` and `/ops/ready`, ignoring `HealthChecks`). `ops_router` is located in `shutdown.rs` despite having nothing to do with graceful shutdown, is not used anywhere in the codebase or test suite, and confuses developers regarding which endpoint to use for kubernetes/monitoring probes.

**Suggested fix:**
Deprecate or remove `ops_router` in favor of the canonical `health_routes()` API. If a standalone ops router without `AppContext` is required for minimal apps, move it to `rume/src/routing/health.rs` as `minimal_health_routes(availability: Availability)` and align the route paths (`/health/live`, `/health/ready`).

---

### m2 Misplaced tracing and ops helpers in `shutdown.rs` violate subsystem cohesion

**Where:** `rume/src/app/shutdown.rs` lines 16–19 and lines 58–88.

**What it says:**
`init_tracing()` and `ops_router()` are defined inside `rume/src/app/shutdown.rs`, whose module doc comment states: `"//! Shutdown signal handling, operational routes, and tracing initialization."`

**Why it's a problem:**
`shutdown.rs` has a clear responsibility: OS signal handling (`shutdown_signal`) and graceful termination coordination. Tracing initialization belongs in `rume/src/app/observability.rs` (which already defines structured spans, request logging, and `ObservabilityLayer`), and ops routes belong in `rume/src/routing/health.rs`. Placing unrelated initialization routines into `shutdown.rs` degrades subsystem cohesion and discoverability.

**Suggested fix:**
Move `init_tracing()` to `rume/src/app/observability.rs` (and provide `try_init_tracing()` as well), move `ops_router` to `rume/src/routing/health.rs` (or deprecate it), leaving `shutdown.rs` solely focused on signal handling and graceful drain.

---

### m3 Stale crate architecture doc comment in `rume/src/lib.rs`

**Where:** `rume/src/lib.rs` lines 3–5.

**What it says:**

```rust
//! This crate is the core of the Rume framework. It is intentionally small:
//! optional features (auth, mail, jobs, storage, database helpers) live in
//! separate `rume-*` crates that depend on this one.
```

**Why it's a problem:**
The comment states that optional features like auth, jobs, storage, and database helpers live in separate `rume-*` crates. In reality, `rume` v02 embeds `auth`, `jobs`, `persistence`, `coordination`, `events`, `outbox`, `saga`, `tenant`, etc. directly inside the `rume` core crate. Per `.opencode/skills/review-authoring/SKILL.md` ("A rationale that is stale — describing a decision the code no longer makes — is itself a finding"), this misleads developers about the project's repository structure.

**Suggested fix:**
Update `lib.rs` module-level documentation to accurately describe Rume's architecture: a batteries-included, modular application framework for Axum where domain subsystems live in first-class internal modules.

---

### m4 `init_tracing()` panics on already-initialized subscriber rather than providing fallible alternative

**Where:** `rume/src/app/shutdown.rs` lines 16–19.

**What it says:**

```rust
pub fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    fmt::fmt().with_env_filter(filter).init();
}
```

**Why it's a problem:**
`.init()` panics if a default tracing dispatcher is already registered in the process. When running integration tests in parallel or embedding Rume in an application with existing tracing setup, calling `init_tracing()` causes an unexpected panic.

**Suggested fix:**
Provide `pub fn try_init_tracing() -> Result<(), tracing::subscriber::SetGlobalDefaultError>` alongside `init_tracing()`, or use `.try_init().ok()` with a fallback log/doc comment explaining the behavior when a subscriber is already installed.

---

### m5 `RumeBuilder::state()` cannot transition state generic `S` on `Rume::new()`

**Where:** `rume/src/app/builder.rs` lines 203–209 and lines 619–623.

**What it says:**

```rust
    pub fn state(mut self, state: S) -> Self {
        self.ctx.state = state.clone();
        self.state = state;
        self
    }
```

**Why it's a problem:**
On `Rume::new()`, `RumeBuilder` is typed as `RumeBuilder<(), (), ()>`. Because `state()` takes `state: S` (which is `()`), calling `.state(MyCustomState)` on a `Rume::new()` builder fails to compile with a type mismatch error (`expected (), found MyCustomState`). A user attempting to progressively add global application state cannot do so through `Rume::new().state(s)`, forcing them to rewrite the entry point to `AppContext::builder((), ()).state(s).build()` and `Rume::builder(ctx)`.

**Suggested fix:**
Clarify in doc comments on `Rume::new()` and `RumeBuilder::state()` that `Rume::new()` is designed for modular state encapsulation (where each module encapsulates its own scoped state), and that applications requiring global state in `AppContext` must initialize via `AppContext::builder` and `Rume::builder(ctx)`.

## Positive notes

- **Explicit Progressive Complexity Model**: `Rume::new()` provides an authentic 3-line progressive entry point (`Rume::new().get("/", handler).run().await`) that starts with default paths, availability, lifecycle, and health checks, yet scales cleanly to first-class modules without changing programming paradigms.
- **First-Class Module Encapsulation**: Unlike frameworks that force all dependencies into a monolithic global state or dynamic DI container, Rume enables domain modules (`UsersModule`, `BillingModule`) to encapsulate their own state privately via `.with_state(...)` on module-owned sub-routers.
- **Rigorous DAG Cycle Detection and Fail-Closed Preflight**: `ModuleRegistry::resolve_startup_order()` implements post-order DFS with 3-color node tracking, catching self-dependencies (`A -> A`) and multi-node cycles (`A -> B -> C -> A`) at preflight before any TCP socket is bound or hooks run. Cycle paths are formatted cleanly.
- **Topological Boot and Reverse-Topological Failure Rollback**: Modules start in strict dependency order and shut down in reverse dependency order. If any module fails during `on_start`, previously started modules are rolled back in reverse topological order within a bounded timeout.
- **No Magic DI or Classpath Scanning**: Rume strictly maintains its architectural refusals: no runtime reflection, no classpath/macro scanning, and no magical DI containers. All dependencies are constructed explicitly in Rust code.
- **Elimination of Third-Party Whole-Crate Re-Exports**: In response to v01 `A-M1`, `lib.rs` and `prelude.rs` completely removed whole-crate leaks (`pub use axum;`, `pub use tokio;`, `pub use serde;`, etc.), decoupling consumers from internal framework dependencies.
- **Strongly Typed Lifecycle and Availability Enums**: Addressing v01 `m2`, `LifecycleState` and `AvailabilityState` now carry `#[repr(u8)]` with explicit discriminant values and bounds checks (`MAX_DISCRIMINANT`).
- **Isolated Testing Harness (`ModuleHarness`)**: Allows testing individual domain modules in isolation with hermetic lifecycles without booting the whole application or binding network ports.

## Verdict

The architecture of Rume v02 is sound, modular, and ready to implement. The progressive complexity model, first-class module composition, topological lifecycle sequencing, and fail-closed DAG cycle detection are excellently designed and adhere strictly to Rume's architectural constraints. Major findings M1 (pruning the over-broad prelude), M2 (enforcing transitive closure in `ArchitectureRules`), and M3 (converting route merge panics into preflight `BuildError`) should be addressed before final production release to ensure optimal API hygiene, CI boundary verification, and fail-closed preflight guarantees.
