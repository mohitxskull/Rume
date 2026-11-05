# Architecture & Module Boundaries Review — v04

## Summary

Rume's core architectural commitments are firmly honored: there is no runtime dependency-injection container, no classpath or module scanning, no hidden `@Transactional`-style AOP, no mandatory ORM or database, `#![forbid(unsafe_code)]` remains intact across the entire codebase, and the framework compiles cleanly in a minimal configuration with `cargo check -p rume --no-default-features`. The first-class domain module system (`Module`), post-order DFS cycle detection, topological startup ordering, and reverse-topological shutdown and rollback provide outstanding modularity and fail-closed preflight safety. However, three major architectural defects prevent an unconditional pass: `ArchitectureRules::verify` ignores undeclared module dependencies and cycles, allowing transitive forbidden domain couplings to pass unnoticed in CI (M1); the core `rume` crate unconditionally depends on `quick-xml` solely for enterprise SAML assertion parsing, violating crate boundary constraints (M2); and `lib.rs` violates re-export discipline by flattening 70+ domain-specific types from all 24 subsystems directly into the `rume::*` crate root (M3). **The subject is not ready to proceed without a fix pass for M1–M3.**

Verification executed:
- `cargo check -p rume --no-default-features` → **passed (clean compilation without sqlx/redis)**
- `cargo check -p rume-cli` → **passed**
- `cargo check -p rume --all-features` → **passed**
- `cargo test -p rume --test integration -- architecture composition` → **15 passed; 0 failed**
- Static source audit of `crates/core/src/lib.rs`, `src/prelude.rs`, `src/module/*`, `src/config.rs`, `src/env/*`, `src/helpers/*`, and workspace `Cargo.toml` manifests.

---

## Prior decisions

Every prior finding across v01 and v02 in this lens's area is classified below using this review's mandatory terms (**REAFFIRMED**, **CHALLENGED**, or **NOTE-NEW**).

> [!IMPORTANT]
> **No v03 lens covered Architecture & Module Boundaries.** The lens was omitted in the v03 cycle and has been restored in v04 per §1.1 and §3.1 of `plan.md`.

### From v01 (`docs/reports/reviews/v01/architecture.md`)

- **v01 M1 / A-M1 (Public API surface leaks unrelated third-party crates; prelude too broad)** — **REAFFIRMED**. Verified in `crates/core/src/lib.rs` and `crates/core/src/prelude.rs`. Whole third-party crate re-exports (`pub use axum;`, `pub use tokio;`, `pub use serde;`, `pub use serde_json;`, `pub use tracing;`) remain expunged from `lib.rs`. In `prelude.rs:17–20`, only standard extractors and derives (`Json`, `Path`, `State` from Axum, `Deserialize`, `Serialize` from Serde) remain. (The internal breadth of `lib.rs` re-exports is evaluated under new finding **M3**).
- **v01 M2 / A-M2 (`Rume::builder` documented as sole entry point, but `RumeBuilder::new` is public)** — **REAFFIRMED**. Verified in `crates/core/src/app/builder.rs:295, 310`. `RumeBuilder::new` is `pub(crate)`. The only public application entry points are `Rume::builder(ctx)` (for typed custom context) and `Rume::new()` (for progressive defaults).
- **v01 m1 / v02 m4 (`init_tracing()` silently discards initialization failure or panics)** — **REAFFIRMED**. Verified in `crates/core/src/app/observability.rs:26–43`. `try_init_tracing()` returns `Result<(), Box<dyn Error + Send + Sync>>`, and `init_tracing()` safely delegates via `let _ = try_init_tracing();` without panicking when a global subscriber is already installed (verified in unit test `tracing_initializer_idempotency_and_try_init`).
- **v01 m2 (Lifecycle/Availability atomic state coupled to declaration order)** — **REAFFIRMED**. Verified in `crates/core/src/app/state.rs:47–54, 114–121`. `LifecycleState` and `AvailabilityState` are `#[repr(u8)]` with pinned explicit discriminants (0..=4), and out-of-range discriminants map safely to `Created` / `Unknown`.

### From v02 (`docs/reports/reviews/v02/architecture.md`, `testing.md`, `todo.md`)

- **v02 M1 (Over-broad `prelude.rs` leaks niche enterprise schemas, coordination locks, and string utilities)** — **REAFFIRMED**. Verified in `crates/core/src/prelude.rs:1–20`. `prelude.rs` was pruned from 70+ types down to 20 lines containing only core application bootstrap, routing, testing, validation, and standard extractors. All enterprise schemas (`SCIM_*`, `Saml*`), distributed coordination locks, background jobs, outbox relays, and string formatting helpers were removed from `prelude.rs`. (Residual test-harness leakage in `prelude.rs` is filed as **m1**).
- **v02 M2 / v02 Testing M2 (`ArchitectureRules::verify` transitive reachability, undeclared modules, cycles)** — **CHALLENGED (defective/incomplete remediation)**. The transitive closure for registered modules was implemented using BFS in `crates/core/src/module/architecture.rs:84–121` and verified by test `architecture_rules_enforce_transitive_dependencies`. However, the remediation failed to address undeclared dependencies and cycles: if a module declares a dependency on a module that is omitted from the `modules` slice passed to `verify()`, `verify` silently terminates traversal at that node (`dep_graph.get(current)` yields `None`), allowing forbidden transitive dependencies reachable through that unprovided node to bypass CI verification completely. Furthermore, `verify()` still does not perform cycle detection. Filed as new finding **M1**.
- **v02 M3 (Route merge collisions trigger runtime panics instead of failing preflight)** — **REAFFIRMED**. Verified in `crates/core/src/app/builder.rs:964–984`. `seen_routes` hash set tracks all `(method, path)` pairs across `route_registry` and all registered modules. Any collision returns `Err(BuildError::DuplicateRoute { method, path })` fail-closed before any call to Axum's `Router::merge`. Verified by integration test `route_merge_collision_fails_preflight_without_panic`.
- **v02 m1 (Orphaned `ops_router` duplicates `health_routes` with divergent paths)** — **REAFFIRMED**. Verified in `crates/core/src/app/shutdown.rs:42–46`. `ops_router` is marked `#[deprecated(since = "0.2.0", note = "Use canonical `rume::routing::health_routes` instead")]`. (Its presence at crate root is reviewed in **m3**).
- **v02 m2 (Misplaced tracing and ops helpers in `shutdown.rs`)** — **REAFFIRMED**. Verified: `init_tracing()` and `try_init_tracing()` moved to `crates/core/src/app/observability.rs`.
- **v02 m3 (Stale crate architecture doc comment in `lib.rs`)** — **REAFFIRMED**. Verified in `crates/core/src/lib.rs:1–6`. The comment claiming optional features live in separate `rume-*` crates was replaced with an accurate description of Rume as a progressive, modular application framework.
- **v02 m5 (`RumeBuilder::state()` cannot transition state generic `S` on `Rume::new()`)** — **CHALLENGED**. Unfixed. The doc comment on `Rume::new()` in `crates/core/src/app/builder.rs:208` continues to advertise that `"Additional modules, state, routes, and middleware can be composed progressively"`, but calling `.state(my_state)` on `Rume::new()` fails to compile due to type pinning (`expected (), found MyState`). Filed as new finding **m2**.
- **v02 Testing M4 (`TestApp` bypasses module lifecycle hooks)** — **REAFFIRMED**. Verified in `crates/core/src/testing/app.rs:37–46` and `crates/core/src/app/builder.rs:567–695`. `TestApp::from_builder(builder).await` executes module `on_start` hooks in topological order, registers reverse teardown hooks, and handles startup rollback on failure. Verified by test `test_app_from_builder_executes_module_lifecycle`.
- **v02 Testing m1 (`ModuleHarness` lacks health check inspection and error-path coverage)** — **REAFFIRMED**. Verified in `crates/core/src/module/harness.rs:76–87`. `health_checks()` and `run_health_checks()` are implemented and verified by test `module_harness_health_checks_and_error_paths`.
- **v02 DX M3 (Configuration provenance reporting)** — **REAFFIRMED**. Verified in `crates/core/src/config.rs:20–65, 152–192`. `ConfigSourceMap` and `ConfigKeyExplanation` track winning files per dotted key, with `load_with_provenance` and `source_map`.
- **v02 DX M4 (Environment interpolation error formatting hides variable name)** — **REAFFIRMED**. Verified in `crates/core/src/env/error.rs:23`. `#[error("failed to interpolate ${{{variable}}}: variable not found")]` formats as `${VAR}` rather than literal `${{variable}}`.

---

## Findings

No Critical findings. The refusal constraints, runtime safety, and topological boot ordering hold. Every defect below affects CI architecture enforcement, dependency boundaries, public surface hygiene, or API contract alignment.

### M1 `ArchitectureRules::verify` ignores undeclared module dependencies and cycles, allowing transitive forbidden dependencies to bypass CI inspection

- **Where:** `crates/core/src/module/architecture.rs:84–123`.
- **What it says:**
  ```rust
  // 2. Build dependency graph and compute transitive closure
  let mut dep_graph: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
  for module in modules {
      dep_graph.insert(module.name(), module.dependencies());
  }

  let mut transitive_deps: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
  for module in modules {
      let mut visited = BTreeSet::new();
      let mut queue = VecDeque::new();
      for dep in module.dependencies() {
          queue.push_back(dep);
      }

      while let Some(current) = queue.pop_front() {
          if visited.insert(current)
              && let Some(next_deps) = dep_graph.get(current)
          {
              for next in next_deps {
                  queue.push_back(next);
              }
          }
      }
      transitive_deps.insert(module.name(), visited);
  }
  ```
- **Why it's a problem:**
  1. *Transitive bypass via unprovided modules*: `verify` constructs `dep_graph` solely from the slice of `modules` passed into it. If a module declares a dependency on a module that is not included in the `modules` slice (e.g. `billing` depends on `"orders"`, but `"orders"` was omitted from the test slice), `dep_graph.get("orders")` returns `None`. The BFS loop inserts `"orders"` into `visited` and silently stops traversing. If `"orders"` in turn depends on `"database_internal"` (a forbidden target), the forbidden path (`billing -> orders -> database_internal`) is completely skipped, producing a false-clean `Ok(())` in CI.
  2. *Missing dependency silence*: Unlike `ModuleRegistry::resolve_startup_order()` (which fails closed with `ModuleError::MissingDependency` if any dependency is absent from the registry), `ArchitectureRules::verify` returns `Ok(())` even if a module declares dependencies that do not exist at all in the verified set.
  3. *Cycle blindness*: If the verified modules contain circular dependencies (`A -> B -> A`), `ArchitectureRules::verify` terminates without error because `visited.insert` prevents infinite loops, but it emits no violation for the cycle.
  
  Because `ArchitectureRules` is explicitly designed for CI verification (`//! CI module dependency graph verification and architectural boundary assertions`), allowing incomplete or cyclic module sets to pass inspection creates a false sense of architectural safety.
- **Suggested fix:**
  In `ArchitectureRules::verify`:
  1. Validate that every dependency named in `module.dependencies()` exists in `registered_names`. If any dependency is missing from the module set, return `Err(ArchitectureViolation { rule: "undeclared_dependency".to_string(), details: format!("module '{}' depends on '{}', which is missing from the verified module set", module.name(), dep) })`.
  2. Perform cycle detection across `modules` (or reuse `ModuleRegistry::resolve_startup_order`) and return `Err(ArchitectureViolation { rule: "circular_dependency".to_string(), details: ... })` if a cycle is detected.
  3. Add test cases in `crates/core/tests/integration/architecture.rs` verifying that unprovided dependencies and circular dependencies fail `verify()`.

---

### M2 Core crate unconditionally depends on `quick-xml` for enterprise SAML parsing, violating core crate boundary constraints

- **Where:** `crates/core/Cargo.toml:46` and `crates/core/src/enterprise/saml.rs:4–5`.
- **What it says:**
  `crates/core/Cargo.toml` lines 46–54:
  ```toml
  quick-xml = "0.42.0"

  [features]
  default = ["sqlite"]
  postgres = ["dep:sqlx", "sqlx/postgres"]
  redis = ["dep:deadpool-redis", "dep:redis"]
  sqlite = ["dep:sqlx", "sqlx/sqlite"]
  mysql = ["dep:sqlx", "sqlx/mysql"]
  ```
  `crates/core/src/enterprise/saml.rs` lines 4–5:
  ```rust
  use quick_xml::events::Event;
  use quick_xml::reader::NsReader;
  ```
- **Why it's a problem:**
  Rule `.opencode/rules/60-architecture.md` specifies non-negotiable crate boundary constraints:
  > `rume` is the core and must not depend on heavy optional crates.
  
  An exhaustive grep across the entire codebase confirms that `quick-xml` is used in exactly one file: `crates/core/src/enterprise/saml.rs`. When compiling `rume` with `--no-default-features`, heavy dependencies like `sqlx`, `redis`, and `deadpool-redis` are excluded, yet `quick-xml` is compiled unconditionally. A consumer building a lightweight JSON API or proxy on `rume` without enterprise SSO is forced to compile `quick-xml` and its transitive dependencies, violating core dependency hygiene.
- **Suggested fix:**
  Make `quick-xml` an optional dependency in `crates/core/Cargo.toml`:
  ```toml
  quick-xml = { version = "0.42.0", optional = true }

  [features]
  saml = ["dep:quick-xml"]
  ```
  Gate `crates/core/src/enterprise/saml.rs` and its re-exports in `crates/core/src/enterprise/mod.rs` and `crates/core/src/lib.rs` behind `#[cfg(feature = "saml")]`.

---

### M3 Crate root `lib.rs` violates re-export discipline by flattening 70+ domain-specific types from all 24 subsystems into `rume::*`

- **Where:** `crates/core/src/lib.rs:35–123`.
- **What it says:**
  `crates/core/src/lib.rs` re-exports dozens of subsystem-specific types, protocol schemas, and leaf helpers directly at the crate root:
  ```rust
  pub use enterprise::{
      parse_iso8601_strict, SamlAssertion, SamlError, SamlParser, SamlSignatureVerifier,
      ScimEmail, ScimErrorResponse, ScimGroup, ScimListResponse, ScimMember, ScimMeta, ScimName,
      ScimUser, SCIM_ERROR_SCHEMA, SCIM_GROUP_SCHEMA, SCIM_LIST_SCHEMA, SCIM_USER_SCHEMA,
  };
  pub use helpers::{
      camel_case, condense_whitespace, dot_case, escape_html, excerpt, format_bytes, format_duration,
      kebab_case, mask_email, mask_secret, ordinal, parse_bytes, parse_duration, pascal_case,
      safe_equal, sentence_case, slug, snake_case, title_case, to_sentence, truncate,
  };
  pub use coordination::{
      CoordinationError, CoordinationFuture, DistributedLock, LeaderElector, LeaderLease, LockGuard,
  };
  pub use jobs::{
      HeartbeatRegistry, IdempotencyStatus, IdempotencyStore, Job, JobContext, JobError, JobHandler,
      JobQueue, JobRegistry, JobWorker, QueuedJob, RetryPolicy, WorkerStatus,
  };
  pub use saga::{FunctionalStep, Saga, SagaError, SagaFuture, SagaReport, SagaStatus, Step};
  pub use outbox::{
      OutboxError, OutboxMessage, OutboxRelay, OutboxRepository, OutboxStatus, RelayPollResult,
  };
  ```
- **Why it's a problem:**
  In v02, finding M1 rightly condemned polluting `prelude.rs` with domain-specific enterprise schemas, coordination locks, and string helpers. In remediating v02 M1, those types were removed from `prelude.rs`, but they were dumped directly into `lib.rs`, flattening almost the entire surface of all 24 internal modules into `rume::*`.
  
  All 24 subsystems are already public modules (`pub mod enterprise; pub mod helpers; pub mod coordination;` etc.). Re-exporting niche protocol schemas (`rume::SCIM_USER_SCHEMA`, `rume::SamlParser`), string casing helpers (`rume::camel_case`, `rume::kebab_case`), and distributed coordination primitives (`rume::LeaderElector`, `rume::LockGuard`) at `rume::*`:
  1. Creates severe autocomplete noise in IDEs when typing `rume::`.
  2. Blurs the modular domain boundaries that Rume's architecture contracts.
  3. Contradicts the charter requirement for **re-export discipline**.
- **Suggested fix:**
  Restrict `rume::*` root re-exports to universal application bootstrap, routing, and lifecycle essentials: `Rume`, `RumeBuilder`, `AppContext`, `Router`, `Module`, `BuildError`, `AppError`, `RequestId`.
  Remove root re-exports of `enterprise::*`, `helpers::*`, `coordination::*`, `jobs::*`, `outbox::*`, `saga::*`, and `tenant::*` from `lib.rs`. Callers should import domain subsystems from their dedicated namespaces (`use rume::enterprise::scim::ScimUser;`, `use rume::helpers::slug;`, `use rume::coordination::DistributedLock;`).

---

### m1 `prelude.rs` exports test harnesses (`TestApp`, `TestAppBuilder`, `ModuleHarness`) into production application code and `testing` module is ungated

- **Where:** `crates/core/src/prelude.rs:8, 13` and `crates/core/src/lib.rs:32`.
- **What it says:**
  `crates/core/src/prelude.rs` lines 8 and 13:
  ```rust
  pub use crate::module::{Module, ModuleContext, ModuleHarness};
  pub use crate::testing::{TestApp, TestAppBuilder};
  ```
  `crates/core/src/lib.rs` line 32:
  ```rust
  pub mod testing;
  ```
- **Why it's a problem:**
  The prelude is intended for everyday types needed across runtime application code (`use rume::prelude::*;`). Exporting test fixtures (`TestApp`, `TestAppBuilder`, `ModuleHarness`) in the standard application prelude pollutes production handler namespaces with testing tools and extractors. Furthermore, the `testing` module is compiled unconditionally into release builds without a `test-util` feature gate.
- **Suggested fix:**
  Remove `TestApp`, `TestAppBuilder`, and `ModuleHarness` from `rume::prelude`. Developers writing integration tests should explicitly import `use rume::testing::TestApp;` or `use rume::module::ModuleHarness;`. Additionally, gate `pub mod testing;` behind `#[cfg(any(test, feature = "test-util"))]` (or document in `testing/mod.rs` that `TestApp` is a lightweight in-memory router wrapper).

---

### m2 `Rume::new()` doc comment advertises progressive state composition, but `RumeBuilder::state()` rejects state transitions due to type pinning

- **Where:** `crates/core/src/app/builder.rs:208` and `crates/core/src/app/builder.rs:798–802`.
- **What it says:**
  `crates/core/src/app/builder.rs:208`:
  ```rust
  /// The application starts with default paths, availability, lifecycle,
  /// and health checks, and state `()`. Additional modules, state, routes,
  /// and middleware can be composed progressively.
  ```
  `crates/core/src/app/builder.rs:798–802`:
  ```rust
  pub fn state(mut self, state: S) -> Self {
      self.ctx.state = state.clone();
      self.state = state;
      self
  }
  ```
- **Why it's a problem:**
  `Rume::new()` returns `RumeBuilder<(), (), ()>`, fixing the state type `S` to `()`. Calling `.state(MyCustomState)` on `Rume::new()` fails to compile with a type mismatch error (`expected (), found MyCustomState`). Because `RumeBuilder` contains `router: Router<AppContext<E, C, S>>`, transitioning `S` on a builder that already has a typed router would alter the router's state type. Applications requiring global state in `AppContext` must initialize via `AppContext::builder(env, config).state(s).build()` and `Rume::builder(ctx)`. But the doc comment on `Rume::new()` claims that "state can be composed progressively", which misleads developers into writing uncompilable code.
- **Suggested fix:**
  Update the doc comment on `Rume::new()` to clarify that `Rume::new()` is designed for progressive zero-config applications and modular state encapsulation (where modules encapsulate their own private state via `.with_state()` on their sub-routers). Explicitly document that applications requiring global state in `AppContext` must initialize via `AppContext::builder` and `Rume::builder(ctx)`.

---

### m3 `lib.rs` re-exports deprecated `ops_router` at crate root

- **Where:** `crates/core/src/lib.rs:35–36`.
- **What it says:**
  ```rust
  #[allow(deprecated)]
  pub use app::ops_router;
  ```
- **Why it's a problem:**
  `ops_router` was deprecated in v02 with the note `"Use canonical rume::routing::health_routes instead"`. Leaving deprecated functions re-exported directly at the top level of `rume::*` with `#[allow(deprecated)]` suppresses compiler deprecation warnings for users importing from `rume::ops_router`, defeating the deprecation notice.
- **Suggested fix:**
  Remove `pub use app::ops_router;` from `lib.rs`. It remains accessible via `rume::app::ops_router` where the deprecation warning will properly fire for any remaining legacy callers.

---

### m4 Missing stability tiers and experimental status documentation in public API docs

- **Where:** `crates/core/src/lib.rs:1–7`.
- **What it says:**
  Module documentation provides a high-level marketing overview of Rume without distinguishing the frozen v1 core contract from experimental subsystems.
- **Why it's a problem:**
  Rule `.opencode/rules/60-architecture.md` specifies:
  > - The v1 contract in `docs/specs/vision/README.md` §7 is frozen.
  > - Everything outside it is experimental and may break.
  
  The lead crate documentation in `lib.rs` does not communicate which subsystems (e.g. core HTTP shell, module system, typed config) are covered by the frozen contract versus experimental features (e.g. sagas, enterprise SAML/SCIM).
- **Suggested fix:**
  Add a `# Stability Tiers` section to `crates/core/src/lib.rs` clarifying that the core application shell, module lifecycle, and configuration are frozen pre-1.0 contracts, while enterprise protocols and advanced distributed coordination remain experimental.

---

## Positive notes

- **Strict Adherence to Refusal Constraints**: Rume remains completely free of runtime DI containers, classpath or module scanning, hidden AOP proxies, and mandatory ORM dependencies. `#![forbid(unsafe_code)]` is strictly enforced across the entire repository with zero unsafe blocks.
- **Fail-Closed Route Merge Collision Preflight**: Preflight route checking in `RumeBuilder::build_router` (lines 964–984) prevents Axum runtime panics by validating route uniqueness across modules and application routes into structured `BuildError::DuplicateRoute` errors before any TCP socket is bound.
- **Deterministic DAG Cycle Detection & Startup Ordering**: `ModuleRegistry::resolve_startup_order` uses post-order DFS with three-color node states to detect cycles (`A -> B -> A`, self-dependencies) and guarantees that dependencies start before dependents.
- **Reverse Topological Shutdown & Failure Rollback**: Modules shut down in strict reverse dependency order. If any module fails during `on_start`, previously started modules are rolled back in reverse topological order within a bounded shutdown timeout.
- **Isolated Testing via `ModuleHarness` & `TestApp::from_builder`**: Domain modules can be tested in complete isolation using `ModuleHarness`, executing their lifecycle hooks and health checks without booting the full application, while `TestApp::from_builder` executes the full module lifecycle in memory.
- **Clean Env/Config Split with Provenance**: `rume::env` and `rume::config` remain cleanly decoupled systems. Figment TOML configuration supports layered overrides with winning provenance tracking via `ConfigSourceMap::explain`, and environment interpolation errors format clearly with the missing variable name.
- **High-Quality Leaf Helpers**: The pure leaf utilities in `rume::helpers` (`safe_equal`, `mask_secret`, `format_bytes`, `parse_duration`, `slug`) are generic, UTF-8 safe, zero-unsafe, and free of domain-specific logic.

---

## Verdict

**Not ready to proceed as-is.**

The core architecture, refusal constraints, module lifecycle, and configuration mechanics are robust and well-engineered. However, shipping or stabilizing before addressing **M1–M3** would:
1. Compromise CI architecture verification by allowing undeclared dependencies and cycles to bypass `ArchitectureRules::verify` (M1).
2. Compromise core crate boundary hygiene by forcing minimal microservices to compile `quick-xml` for unused enterprise SAML assertions (M2).
3. Compromise public API stability and developer experience by polluting `rume::*` with 70+ domain-specific and protocol-level types (M3).

A focused fix pass addressing M1–M3 will bring Rume's architecture into full alignment with its vision and hard constraints.
