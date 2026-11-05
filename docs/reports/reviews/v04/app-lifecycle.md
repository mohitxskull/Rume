# Application Lifecycle & Composition Review — v04

## Summary

Rume's application lifecycle and progressive composition subsystem demonstrates mature, disciplined design: `Rume::new()` provides an authentic progressive entry point, `AppContext::builder` seamlessly supports non-`Default` application state without type annotations, `ObservabilityLayer` safely injects CSPRNG UUID `request_id` values into RFC 9457 Problem Details without payload corruption, and graceful shutdown correctly transitions availability to `AvailabilityState::RefusingTraffic` before in-flight HTTP connections drain. All 45 integration tests in this lens's scope pass (`cargo test -p rume --test integration -- lifecycle composition health http_smoke route_metadata request_identity` → 45 passed, 0 failed). However, **5 Major** design and composition defects prevent production certification: `RumeBuilder::openapi()` freezes the route registry eagerly at configuration time, permanently omitting 100% of module routes from generated OpenAPI documentation; `build_test_app()` inverts shutdown hook execution order relative to `run()` and omits application cleanup hooks on module startup failure; the shutdown signal is completely unmonitored during startup hooks (leaving booting processes unresponsive to SIGTERM/Ctrl+C); module route collision detection fails to account for `ANY` matcher overlap, triggering unhandled runtime panics in Axum; and `Availability::set` is public and unconstrained, allowing in-flight handlers or background workers to overwrite `RefusingTraffic` during graceful drain. **Not ready to ship as-is; a targeted fix pass for M1–M5 is required.**

---

## Prior decisions

> [!IMPORTANT]
> **No v03 lens covered Application Lifecycle & Composition.** This surface was omitted in v03 and v03/diff/v01; all prior decisions audited below originate from v01 and v02. Per §3.2 of the review plan, every prior finding in this lens's scope is classified below as REAFFIRMED, CHALLENGED, or NOTE-NEW.

| Prior finding | Disposition | Evidence & Analysis |
|---|---|---|
| **v01 C2 — `ApplicationMode::Inspect` still serves HTTP traffic** (`v01/correctness.md:40–46`) | **REAFFIRMED** | `crates/core/src/app/builder.rs:1108–1129, 1190–1211`: In `Inspect` mode, the context transitions to `LifecycleState::Booted` and `AvailabilityState::Live`, outputs route metadata when requested, and immediately returns `Ok(())` without binding listeners or serving traffic. Executed: unit tests `inspect_mode_sets_booted_live_then_returns`, `inspect_mode_does_not_run_on_ready_hooks`, and `inspect_mode_does_not_run_on_shutdown_hooks` pass. |
| **v01 C3 — `Lifecycle::set` / `Availability::set` accept any `u8`** (`v01/correctness.md:48–54`) | **REAFFIRMED** | `crates/core/src/app/state.rs:77–101, 144–167`: Discriminants are pinned with `#[repr(u8)]` and `MAX_DISCRIMINANT = 4`. Out-of-range memory reads map deterministically to `Created` and `Unknown`. Executed: unit tests `lifecycle_invalid_u8_maps_to_created` and `availability_invalid_u8_maps_to_unknown` pass. *(Residual state machine transition flaws are filed as NOTE-NEW M5).* |
| **v01 m3 — `RouteSpec::build` registers in global registry even when unmounted** (`v01/correctness.md:72–78`) | **REAFFIRMED** | `crates/core/src/routing/spec.rs:153–164`: Global `ROUTE_REGISTRY` singleton was eliminated. `RouteRegistry` is strictly application-scoped on `AppContext` and `Router`. `RouteSpec::build()` returns metadata without registration side effects. |
| **v01 positive note — `run()` lifecycle ordering** (`v01/correctness.md:94`) | **REAFFIRMED** | `crates/core/src/app/builder.rs:1261–1390`: Sequence `Booted`/`Live` → (module `on_start`) → (app `on_ready`) → `Ready`/`Ready` → `axum::serve` + graceful shutdown (`Terminating`/`RefusingTraffic`) → (app `on_shutdown`) → (module `on_shutdown`) → `Terminated`/`Unknown` verified. Executed: `lifecycle::graceful_shutdown_transitions_to_refusing_traffic_while_request_in_flight` passes. |
| **v01 positive note — `/health/live` returns 503 only when `Broken`** (`v01/correctness.md:96`) | **REAFFIRMED** | `crates/core/src/routing/health.rs:315–323`: Returns 503 only on `AvailabilityState::Broken`, matching liveness probe contracts. Executed: `health::health_live_returns_ok_when_not_broken` passes. |
| **v02 A-M2 — `RumeBuilder::new` is public** (`v02/architecture.md:10`) | **REAFFIRMED** | `crates/core/src/app/builder.rs:303`: `RumeBuilder::new` is `pub(crate)`. Canonical public entry points are `Rume::builder(ctx)` and `Rume::new()`. |
| **v02 M3 — Route merge collisions trigger unhandled runtime panics** (`v02/architecture.md:69–88`) | **CHALLENGED (defective remediation)** | `crates/core/src/app/builder.rs:964–984`: While direct `(method, path)` module collisions now return `BuildError::DuplicateRoute`, the preflight check keys strictly on `(method.to_string(), path)`. Collisions involving `ANY` routes and direct `Router::merge` / `RumeBuilder::merge` still trigger unhandled runtime panics in Axum. Filed as **M4**. |
| **v02 m1 / DX m1 — Orphaned `ops_router` duplicates `health_routes`** (`v02/architecture.md:90–115`, `v02/dx-tooling.md:251–269`) | **REAFFIRMED** | `crates/core/src/app/shutdown.rs:42–46`: `ops_router` is marked `#[deprecated(since = "0.2.0", note = "Use canonical rume::routing::health_routes instead")]` and was removed from `prelude.rs`. Retained with deprecation annotations to preserve backward compatibility. |
| **v02 m2 — Misplaced tracing and ops helpers in `shutdown.rs`** (`v02/architecture.md:118–131`) | **REAFFIRMED** | `crates/core/src/app/observability.rs:30–43`: Tracing initialization logic moved to `observability.rs`. `shutdown.rs` re-exports them and focuses on signal handling and graceful drain. |
| **v02 m4 — `init_tracing()` panics on already-initialized subscriber** (`v02/architecture.md:153–172`) | **REAFFIRMED** | `crates/core/src/app/observability.rs:30–43`: `try_init_tracing()` returns `Result<(), Box<dyn Error>>`, and `init_tracing()` safely ignores `Err`. Executed: unit test `tracing_initializer_idempotency_and_try_init` passes. |
| **v02 m5 / DX m5 — `RumeBuilder` lacks `.openapi()`, `.metrics()`, state transition** (`v02/architecture.md:174–193`, `v02/dx-tooling.md:334–348`) | **CHALLENGED (defective remediation)** | `crates/core/src/app/builder.rs:739–760`: Methods were added, but `RumeBuilder::openapi()` freezes the route registry eagerly at invocation time, completely omitting all module routes. Filed as **M1**. |
| **v02 D-M1 / M2 — `AppContext::builder` requires `S: Default`** (`v02/dx-tooling.md:74–135`) | **REAFFIRMED** | `crates/core/src/app/context.rs:82–107, 155–171`: `AppContext::builder(env, config)` initializes with state `()`, and `.state(new_state)` transitions type without requiring `S: Default`. Executed: unit test `app_context_builder_non_default_state_and_inference` passes. |
| **v02 T4 — `TestApp`/`TestAppBuilder` test harness is untested** (`v02/testing.md:14`) | **REAFFIRMED** | `crates/core/src/testing/app.rs:180–290`: 7 unit tests explicitly verify HTTP methods, drop hooks, teardown, and status helpers. All pass. |
| **v02 T5 — `app.rs` tests miss boundary, Inspect-mode, lifecycle surface** (`v02/testing.md:15`) | **REAFFIRMED** | `crates/core/src/app/builder.rs:1577–1686`, `crates/core/src/app/state.rs:205–231`, `crates/core/tests/integration/lifecycle.rs`: Comprehensive unit and integration suites cover inspect mode, startup failure rollbacks, and shutdown transitions. |
| **v02 T8 — `http_smoke.rs` tests bare router, not real health surface** (`v02/testing.md:18`) | **REAFFIRMED** | `crates/core/tests/integration/health.rs`: Dedicated 199-line integration suite thoroughly tests `health_routes()` with `AppContext` state, `HealthChecks::run`, degraded `Warning` (503), failed checks (503), and empty `debug_info`. |
| **v02 M4 — `TestApp` / `RumeBuilder::test_app()` bypasses module lifecycle hooks** (`v02/testing.md:126–150`) | **CHALLENGED (defective remediation)** | `crates/core/src/app/builder.rs:567–697`: `build_test_app()` was introduced, but its teardown ordering inverts `run()` sequencing, startup failure skips application `on_shutdown` hooks, and it ignores `ApplicationMode::Inspect`. Filed as **M2**. |

**Areas with no prior coverage (NOTE-NEW):** Unmonitored shutdown signal during startup hooks (M3), public unconstrained `Availability::set` (M5), stale code rationale comment on `RumeBuilder::route` (m1), empty `MatchedPath` in `ObservabilityLayer` spans (m2), rigid `SocketAddr` hostname rejection (m3), `/health/live` returning 200 on `Unknown` (m4), `build_test_app()` ignoring `Inspect` mode (m5), and `TestApp` drop blocking runtime threads (m6).

---

## Findings

### M1 `RumeBuilder::openapi()` freezes route registry eagerly, permanently omitting all module routes and later-registered routes

- **Where:** `crates/core/src/app/builder.rs:739–746` and `crates/core/src/app/builder.rs:1088–1091`
- **What it says:**
  ```rust
  pub fn openapi(self, path: &str, info: crate::openapi::OpenApiInfo) -> Self {
      let spec = self.route_registry.to_openapi(info);
      let json_value = spec.to_json_value();
      self.get(path, move || {
          let val = json_value.clone();
          async move { axum::Json(val) }
      })
  }
  ```
  and in `run()`:
  ```rust
  self.ctx.route_registry = self.route_registry.clone();
  ```
  while `build_router()` extends an ephemeral local copy:
  ```rust
  let mut module_routes = crate::routing::RouteRegistry::new();
  ...
  route_registry.extend(module_routes);
  // local route_registry is discarded when build_router() returns!
  ```
- **Why it's a problem:**
  1. `RumeBuilder::openapi()` calls `self.route_registry.to_openapi(info)` eagerly at builder definition time and moves a static JSON clone into the handler closure.
  2. Because modules only register their routes during `build_router()` (or preflight), `self.route_registry` contains **zero module routes** when `.openapi()` is invoked, even if `.module(...)` was registered prior to `.openapi(...)`.
  3. Any application route added via `.get()`, `.post()`, or `.resource()` *after* `.openapi()` is likewise missing.
  4. In addition, `run()` clones `self.route_registry` into `self.ctx.route_registry` at line 1091 *before* `build_router` is called. Inside `build_router`, `route_registry` is extended with `module_routes`, but that extended registry is discarded when `build_router()` returns. In the running application, `AppContext::route_registry` has zero module routes, breaking runtime introspection.
  In any modular application, the `/openapi.json` endpoint omits 100% of domain module endpoints.
- **Suggested fix:**
  Make OpenAPI generation lazy or defer spec generation until preflight:
  1. In `build_router()`, populate the extended `route_registry` directly into `ctx.route_registry`.
  2. In `RumeBuilder::openapi()`, mount an endpoint that reads `State(ctx): State<AppContext<E, C, S>>` and renders `ctx.route_registry.to_openapi(info)` dynamically (with optional caching via `Arc<RwLock<Option<Value>>>`), guaranteeing that all application and module routes are included regardless of registration order.
- **Evidence:** Static verification: `builder.rs:739–746, 1088–1091, 988`. Executed: `composition::builder_mounts_openapi_and_metrics_endpoints` passed only because it tested routes registered *before* `.openapi()` with zero modules.

---

### M2 `build_test_app()` lifecycle teardown inverts shutdown hook ordering and diverges from `run()` rollback semantics

- **Where:** `crates/core/src/app/builder.rs:602–613, 663–674` vs `crates/core/src/app/builder.rs:1284–1297, 1364–1375`
- **What it says:**
  In `run()` (production drain, lines 1364–1375):
  ```rust
  let on_shutdown_fut = async {
      for hook in on_shutdown {
          hook(serve_ctx.clone()).await;
      }
      for sm in shutdown_modules {
          sm.on_shutdown(&sm_ctx).await;
      }
  };
  ```
  In `build_test_app()` (test harness teardown, lines 663–674):
  ```rust
  for m in shutdown_modules {
      m.on_shutdown(&sm_ctx).await;
  }
  for hook in on_shutdown {
      let _ = hook(cleanup_ctx.clone()).await;
  }
  ```
  Furthermore, on module startup failure, `run()` executes application `on_shutdown` hooks (lines 1294–1296), whereas `build_test_app()` (lines 604–612) omits application `on_shutdown` hooks entirely.
- **Why it's a problem:**
  1. Startup executes in dependency order: modules initialize first (`on_start`), followed by application startup tasks (`on_ready`).
  2. Graceful shutdown must execute in strict reverse order: application cleanup tasks (`on_shutdown`) must run first while module resources remain available, followed by modules in reverse topological order (`shutdown_modules`).
  3. `build_test_app()` runs module shutdown *before* application shutdown hooks. If an application `on_shutdown` hook flushes an outbox, drains background workers, or logs audit events using a database pool or Redis client owned by a module, the module has already terminated its connections, causing the application hook to fail in test environments while succeeding in production.
  4. On module startup failure, `build_test_app()` omits application `on_shutdown` hooks, leaking non-module resources acquired prior to boot.
- **Suggested fix:**
  Align `build_test_app()` with `run()`:
  1. In `build_test_app()`'s teardown closure, execute `on_shutdown` hooks *before* `shutdown_modules`.
  2. When a module fails `on_start` in `build_test_app()`, execute registered `on_shutdown` hooks alongside module rollback, matching lines 1294–1296 of `run_with_bound_listener`.
- **Evidence:** Static verification: code comparison between lines 663–674 and lines 1364–1375.

---

### M3 Shutdown signal is unmonitored during application and module startup hooks, preventing graceful cancellation of hanging boots

- **Where:** `crates/core/src/app/builder.rs:1157–1168` and `crates/core/src/app/builder.rs:1261–1358`
- **What it says:**
  In `run_with_bound_listener`:
  ```rust
  // Step 4: Module startup hooks in topological dependency order
  for m in startup_modules {
      if let Err(err) = m.on_start(&mod_ctx).await { ... }
  }

  // Step 5: Application startup hooks with cleanup on failure
  for hook in on_ready {
      if let Err(err) = hook(serve_ctx.clone()).await { ... }
  }

  // Step 7: Serve traffic with immediate lifecycle & availability transition on shutdown
  let graceful_signal = async move {
      signal.await;
      ...
  };
  axum::serve(listener, router.with_state(serve_ctx.clone()))
      .with_graceful_shutdown(graceful_signal)
      .await;
  ```
- **Why it's a problem:**
  The shutdown `signal` future is passed into `run_with_bound_listener` but is **only polled at Step 7** when `axum::serve` begins. Steps 4 and 5 (executing database migrations, warming caches, establishing broker connections, and running user `on_ready` tasks) await hook futures sequentially without selecting on `signal`.
  If an operator issues `SIGTERM` or `Ctrl+C` while an application is booting (e.g. database migration is blocked waiting for an advisory lock, external service is unreachable), the shutdown signal is completely ignored. The application continues running startup tasks until all hooks finish or fail. If a startup hook hangs, the process hangs indefinitely and cannot be terminated without `SIGKILL`.
- **Suggested fix:**
  Wrap module and application startup loops in `tokio::select!` with a cloned or shared signal notification (e.g. `tokio::sync::watch` or broadcast channel). If a shutdown signal arrives during startup, abort startup immediately, transition to `LifecycleState::Terminating` and `AvailabilityState::RefusingTraffic`, execute rollback and shutdown cleanup hooks, and exit cleanly.
- **Evidence:** Static verification: `signal` argument is unused between lines 1255 and 1350.

---

### M4 Module route collision preflight ignores `ANY` matcher overlap, triggering unhandled Axum runtime panics

- **Where:** `crates/core/src/app/builder.rs:964–984`
- **What it says:**
  ```rust
  let mut seen_routes = HashSet::new();
  for r in route_registry.routes() {
      seen_routes.insert((r.method.to_string(), r.path.clone()));
  }

  for m in &startup_modules {
      let single_mod_router = m.register_routes(crate::routing::Router::new());
      let (axum_r, routes) = single_mod_router.into_parts();
      for r in routes.routes() {
          let key = (r.method.to_string(), r.path.clone());
          if !seen_routes.insert(key) {
              return Err(BuildError::DuplicateRoute {
                  method: r.method.to_string(),
                  path: r.path.clone(),
              });
          }
      }
      module_axum = module_axum.merge(axum_r);
      ...
  }
  ```
- **Why it's a problem:**
  `seen_routes` checks for exact matches of `(method.to_string(), path)`.
  If an application route mounts `builder.any("/api/users", h1)` (`method: "ANY"`) and a module registers `router.get("/api/users", h2)` (`method: "GET"`), or if Module A mounts `ANY` and Module B mounts `POST`:
  1. `seen_routes.insert(("GET", "/api/users"))` succeeds because `("GET", ...)` does not equal `("ANY", ...)`.
  2. The preflight check passes without returning `BuildError::DuplicateRoute`.
  3. When `module_axum.merge(axum_r)` or `router.merge(adapted)` executes, Axum's underlying `Router::merge` panics at runtime with `"Cannot merge two routers that both define a route for: /api/users"`.
  This violates Rume's core contract that route conflicts fail closed through typed `BuildError::DuplicateRoute` during preflight rather than panicking the process.
- **Suggested fix:**
  Update collision detection in `build_router()`: if either route specifies method `"ANY"`, or if their HTTP methods match on the same normalized path, treat them as conflicting and return `Err(BuildError::DuplicateRoute { method, path })`.
- **Evidence:** Static verification: line 973 inserts raw `r.method.to_string()` with no `ANY` wildcard handling.

---

### M5 `Availability::set` is public and unconstrained, allowing handlers to bypass `RefusingTraffic` during graceful drain

- **Where:** `crates/core/src/app/state.rs:159–167` and `crates/core/src/routing/health.rs:327–341`
- **What it says:**
  In `crates/core/src/app/state.rs`:
  ```rust
  pub fn set(&self, state: AvailabilityState) {
      let value = state as u8;
      if value <= Self::MAX_DISCRIMINANT {
          self.inner.store(value, Ordering::SeqCst);
      }
  }
  ```
  while `Lifecycle::set` is restricted:
  ```rust
  pub(crate) fn set(&self, state: LifecycleState)
  ```
- **Why it's a problem:**
  1. Unlike `Lifecycle::set` (which is `pub(crate)`), `Availability::set` is `pub` and operates on `AppContext.availability`, which is accessible to all HTTP handlers and background tasks via `State<AppContext<E, C, S>>`.
  2. `Availability::set` allows unconditional overwrites without checking for terminal or draining states.
  3. When graceful shutdown begins, `run()` transitions availability to `AvailabilityState::RefusingTraffic`, which causes `/health/ready` to return 503 so load balancers withdraw the instance.
  4. While in-flight requests finish draining, any handler or asynchronous task that calls `ctx.availability.set(AvailabilityState::Ready)` will overwrite `RefusingTraffic` with `Ready`. Subsequent readiness probes to `/health/ready` return 200 OK again, tricking orchestrators into routing new user traffic to a terminating instance that is about to terminate its process.
- **Suggested fix:**
  Make `Availability::set` private or `pub(crate)`, or provide a CAS transition method `transition(&self, from: AvailabilityState, to: AvailabilityState) -> Result<(), AvailabilityState>` that rejects transitions out of `RefusingTraffic` or `Broken`. If handlers require reporting health degradation, expose a restricted `mark_broken()` or `set_degraded()` helper that cannot clear `RefusingTraffic`.
- **Evidence:** Static verification: `state.rs:159` is `pub`, whereas line 93 is `pub(crate)`.

---

### m1 Stale code rationale comment on `RumeBuilder::route` contradicts active registration behavior

- **Where:** `crates/core/src/app/builder.rs:701–722`
- **What it says:**
  ```rust
  /// Add a route to the application.
  ///
  /// The route is mounted on the router but not recorded in the route
  /// registry: a [`MethodRouter`] does not expose its HTTP method, so
  /// recording a [`crate::routing::RouteInfo`] here would require fabricating
  /// one (likely `GET`), which would produce incorrect `rume list:routes`
  /// output. Registration stays explicit: construct a [`RouteInfo`] from
  /// [`crate::routing::RouteSpec::build`] metadata and call
  /// [`RouteRegistry::register`] on [`AppContext::route_registry`].
  ///
  /// Why: a wrongly registered method is a real bug for `rume list:routes`,
  /// whereas an empty registry is obvious and easy to populate explicitly.
  /// Revisit if `MethodRouter` exposes its method or `route()` gains method
  /// information.
  pub fn route(mut self, path: &str, method_router: MethodRouter<AppContext<E, C, S>>) -> Self {
      self.router = self.router.route(path, method_router);
      self.route_registry.register(crate::routing::RouteInfo {
          method: axum::http::Method::from_bytes(b"ANY").unwrap(),
          path: path.to_string(),
          name: None,
          params: crate::routing::registry::extract_path_params(path),
          matchers: Vec::new(),
      });
      self
  }
  ```
- **Why it's a problem:**
  The docstring and its explicit `Why:` rationale comment emphatically explain why `route()` does **not** record in `self.route_registry` ("recording a RouteInfo here would require fabricating one... a wrongly registered method is a real bug for `rume list:routes`"). Yet the immediate implementation on lines 715–721 does exactly that: it fabricates method `ANY` and registers the route in `self.route_registry`.
  Per `.opencode/rules/70-code-rationale.md`: *"A rationale that is stale — describing a decision the code no longer makes — is itself a finding. When the decision changes, update or delete its comment in the same edit."* The rationale comment directly misleads readers and auditors.
- **Suggested fix:**
  Update the doc comment and `Why:` rationale to document the active decision: `route()` records `ANY` in the registry so `rume list:routes` captures all mounted paths, while noting that specific method filtering is not available for raw `MethodRouter`s.
- **Evidence:** Static verification: lines 701–712 contradict lines 715–721 in the same function.

---

### m2 `ObservabilityLayer` checks `MatchedPath` outside Axum router, leaving span `route` field permanently empty

- **Where:** `crates/core/src/app/observability.rs:190–212` and `crates/core/src/app/builder.rs:1072`
- **What it says:**
  In `ObservabilityService::call`:
  ```rust
  let matched_path = req
      .extensions()
      .get::<axum::extract::MatchedPath>()
      .map(|mp| mp.as_str().to_string());

  let span = if let Some(ref route) = matched_path {
      tracing::info_span!("http_request", ..., route = %route, ...)
  } else {
      tracing::info_span!("http_request", ..., ...)
  };
  ```
- **Why it's a problem:**
  `ObservabilityLayer` wraps the outer router (`router = router.layer(ObservabilityLayer::new())` at line 1072 of `builder.rs`). When `ObservabilityService::call` is invoked on request entry, the incoming request has not yet traversed Axum's routing tree. Axum inserts `MatchedPath` only when a route matches inside the router. Consequently, `req.extensions().get::<MatchedPath>()` is **always `None`** when the tracing span is created. The span invariably falls into the `else` branch, and the `route` field is never recorded in any request span.
- **Suggested fix:**
  Record `route` on response exit if present in extensions, or mount `ObservabilityLayer` using `axum::middleware::from_fn` / `route_layer` where `MatchedPath` is guaranteed to be available in request extensions.
- **Evidence:** Static verification: `ObservabilityLayer` is applied on the outer router; `MatchedPath` is inserted only during inner route matching.

---

### m3 `RumeBuilder::run` rejects valid hostnames (`localhost`) due to rigid `SocketAddr` parsing

- **Where:** `crates/core/src/app/builder.rs:1093–1096`
- **What it says:**
  ```rust
  let addr: SocketAddr = self
      .bind
      .parse()
      .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidInput, err))?;
  ```
- **Why it's a problem:**
  `IntoBindAddr` accepts `&str`, `String`, and `(&str, u16)`. If a developer writes `Rume::new().bind("localhost:3000")` or `.bind(("localhost", 3000))`, the code compiles cleanly, but `run()` fails at runtime with `InvalidInput: invalid socket address syntax`. Standard Tokio `TcpListener::bind` accepts hostnames and resolves them asynchronously. Forcing synchronous `SocketAddr::from_str` prevents binding to hostnames like `localhost`.
- **Suggested fix:**
  Resolve addresses asynchronously using `tokio::net::lookup_host(&self.bind).await` or pass `self.bind` directly to `TcpListener::bind(&self.bind).await`.
- **Evidence:** Static verification: `SocketAddr::from_str` rejects hostnames like `"localhost:3000"`.

---

### m4 `/health/live` returns 200 OK for `AvailabilityState::Unknown`, reporting terminated or unbooted applications as alive

- **Where:** `crates/core/src/routing/health.rs:315–323`
- **What it says:**
  ```rust
  if ctx.availability.state() == AvailabilityState::Broken {
      (StatusCode::SERVICE_UNAVAILABLE, Json(json!({"status": "broken"}))).into_response()
  } else {
      (StatusCode::OK, Json(json!({"status": "alive"}))).into_response()
  }
  ```
- **Why it's a problem:**
  `AvailabilityState` initializes to `Unknown = 0`, and `run()` resets availability to `Unknown` upon post-drain termination. Because `/health/live` returns 503 *only* for `AvailabilityState::Broken`, an application in `Unknown` state (e.g. before `on_start` runs, after termination, or if an invalid `u8` falls back to `Unknown`) returns `200 OK {"status": "alive"}`.
- **Suggested fix:**
  Require `AvailabilityState::Live | AvailabilityState::Ready | AvailabilityState::RefusingTraffic` for `200 OK` on `/health/live`, and return `503 Service Unavailable` when availability is `Unknown` or `Broken`.
- **Evidence:** Static verification: `health.rs:315` checks only `== Broken`.

---

### m5 `build_test_app()` completely ignores `ApplicationMode::Inspect`

- **Where:** `crates/core/src/app/builder.rs:567–697`
- **What it says:**
  `pub async fn build_test_app(mut self) -> Result<crate::testing::TestApp, BuildError>` never reads `self.mode`.
- **Why it's a problem:**
  In `run()`, when `mode == ApplicationMode::Inspect`, the application sets `Booted`/`Live`, skips all startup and shutdown hooks, and exits immediately. In `build_test_app()`, `self.mode` is completely ignored; even if configured with `ApplicationMode::Inspect`, it executes all module startup hooks and application `on_ready` hooks.
- **Suggested fix:**
  In `build_test_app()`, check `self.mode == ApplicationMode::Inspect`; if true, transition to `Booted`/`Live` and return `TestApp` without executing hooks.
- **Evidence:** Static verification: `mode` is never referenced in `build_test_app()`.

---

### m6 `TestApp` drop blocks Tokio runtime thread via OS thread `join`

- **Where:** `crates/core/src/app/builder.rs:680–691` and `crates/core/src/testing/app.rs:113–121`
- **What it says:**
  ```rust
  let _ = std::thread::spawn(move || {
      if let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() {
          rt.block_on(async {
              let _ = tokio::time::timeout(shutdown_timeout, shutdown_future).await;
          });
      }
  })
  .join();
  ```
- **Why it's a problem:**
  When `TestApp` is dropped at the end of an async test, its `Drop` implementation executes synchronous cleanup by spawning an OS thread and blocking on `.join()`. If executed inside Tokio's multi-threaded runtime, this blocks the worker thread for the duration of teardown (up to `shutdown_timeout`, which defaults to 30s if a hook hangs), risking test executor stalls.
- **Suggested fix:**
  Provide an explicit `pub async fn teardown_async(mut self)` method on `TestApp` that awaits teardown on the active runtime, and reserve the synchronous drop guard as a best-effort fallback.
- **Evidence:** Static verification: `builder.rs:680–691` calls `std::thread::spawn(...).join()`.

---

## Positive notes

- **Progressive Composition Architecture:** `Rume::new()` authentically delivers a 3-line progressive entry point that defaults cleanly to sensible paths, availability, lifecycle, and health checks, yet scales smoothly to first-class modules without changing programming models.
- **Safe Problem Details Request ID Injection:** `ObservabilityService` (`crates/core/src/app/observability.rs:243–314`) inspects RFC 9457 Problem Details responses with strict 1 MiB bounds and streaming safety: oversized payloads are returned untouched without truncation, and only valid JSON bodies with missing/null `request_id` are modified, safely updating `Content-Length`.
- **Non-Panicking Tracing Initialization:** Addressing v02 finding m4, `init_tracing()` (`observability.rs:41–43`) uses `try_init_tracing()` to safely ignore duplicate subscriber installations during multi-threaded test runs and reboots.
- **Topological Boot and Reverse-Topological Failure Rollback:** In `run()`, modules boot in strict DAG dependency order and shut down in reverse order. If any module fails during `on_start`, previously started modules are rolled back in reverse order bounded by `shutdown_timeout`.
- **Readiness-Before-Drain Graceful Shutdown:** When a shutdown signal is received, availability immediately transitions to `AvailabilityState::RefusingTraffic`, causing `/health/ready` to return 503 while in-flight requests finish draining.
- **Disciplined Middleware Onion Execution:** Middleware registration ordering and group member ordering are executed in strict accordance with the onion model (outermost individual middleware -> group middleware -> inner handler -> group exit -> individual exit), validated by unit test `middleware_combined_individual_and_groups_execution_order`.
- **AppContextBuilder Ergonomics:** Addressing v02 finding M2, `AppContext::builder` allows seamless initialization with `AppContext::builder(env, config).state(custom_state).build()` without generic inference failure or `S: Default` restrictions.

---

## Verdict

**Not ready to proceed / ship as-is.**

The core application architecture, progressive builder ergonomics, and basic graceful shutdown sequencing are well-engineered and sound. However, **5 Major** design and composition defects prevent production certification:

1. **M1** — Fix `RumeBuilder::openapi()` and `run()` route registry propagation so that domain module endpoints are included in generated `/openapi.json` documentation and runtime route introspection.
2. **M2** — Align `build_test_app()` with `run()` so application `on_shutdown` hooks execute before module shutdown, and execute cleanup hooks on module startup failure.
3. **M3** — Monitor the shutdown signal during module `on_start` and application `on_ready` startup hooks to allow cancellation of hanging boots.
4. **M4** — Handle `ANY` route matcher overlap during module preflight to prevent unhandled runtime panics in Axum's `Router::merge`.
5. **M5** — Restrict `Availability::set` to prevent in-flight handlers from clearing `RefusingTraffic` during graceful drain.

Resolving findings M1–M5 alongside polish items m1–m6 will make Rume's application lifecycle and composition rock-solid.
