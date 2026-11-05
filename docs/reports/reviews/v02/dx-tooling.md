# DX & Tooling Review — v02

## Summary

Rume v02 achieves noteworthy improvements in its developer experience: `rume new` now generates fully portable projects depending on released versions by default, CLI inspection commands default to redacting secrets with machine-readable JSON output, generated code directly imports `axum` and `serde`, and the `rume::helpers` module provides a comprehensive suite of UTF-8 safe utility functions. However, significant ergonomics and tooling gaps remain: OpenAPI schema extraction ignores parameter matchers and relies on crude parameter-name substring heuristics (mapping UUIDs to integers and numbers to strings); `AppContext::builder` suffers from generic inference failures and an unnecessary `where S: Default` bound that forces template scaffolding and developers back to the 6-positional-arg constructor; configuration provenance reporting is completely missing despite layered configuration claims; environment interpolation error messages literally print `${variable}` rather than the variable name; and the scaffolded `README.md` instructs users to visit a 404 `/health` endpoint while `routes.rs.tpl` squats `/users` and `/posts` with dummy health handlers. A fix pass addressing these design and tooling defects is required before release.

---

## Prior decisions

All prior findings recorded in `docs/reports/reviews/v01/dx-usability.md` (and summarized in `docs/reports/reviews/v01/todo.md`) have been re-evaluated against the current implementation:

- **D-M1 — `AppContext::new()` takes 6 positional args of mixed types** — **CHALLENGED**. While `AppContext::builder` was introduced in `crates/core/src/app/context.rs:85–170`, the builder fails to infer the state generic parameter `S` without explicit type annotations (`AppContext::<_, _, ()>::builder(...)`), and `AppContextBuilder::build()` requires `where S: Default`. As a result, non-`Default` application state cannot use the builder, and the generated project template (`rume-cli/templates/src/app.rs.tpl:23–30`) still calls the 6-positional-arg `AppContext::new(...)` constructor. Evaluated under finding M2.
- **D-M2 — `Rume::builder()` documented as sole entry point, but `RumeBuilder::new()` is public** — **REAFFIRMED**. Verified in `crates/core/src/app/builder.rs:295`. `RumeBuilder::new` was made `pub(crate)`, establishing `Rume::builder(ctx)` (for typed custom context) and `Rume::new()` (for 3-line progressive defaults) as the canonical public entry points.
- **D-M3 — `health_routes()` and `ops_router()` are two overlapping health-endpoint APIs** — **CHALLENGED**. Rather than unifying the APIs into one canonical health check interface, `ops_router` was retained in `crates/core/src/app/shutdown.rs:58–88` and merely re-routed to `/ops/live` and `/ops/ready` on `Router<Availability>`, ignoring `HealthChecks`. It remains an orphaned duplicate operational route API with an incompatible state model, re-exported in `prelude.rs:7`. Evaluated under finding m1.
- **D-m1 — Prelude re-exports whole third-party crates, polluting the namespace** — **REAFFIRMED**. Verified in `crates/core/src/lib.rs:34–100` and `crates/core/src/prelude.rs:75–78`. Whole third-party crate re-exports (`pub use axum;`, `pub use tokio;`, `pub use serde;`, etc.) have been completely removed. (Note: Prelude breadth regarding internal subsystem types is evaluated in Architecture v02 M1).
- **D-m2 — `rume generate handler` uses fragile string-marker codegen with silent degradation** — **REAFFIRMED** with new finding. The generator now terminates with `std::process::exit(1)` when `// rume:routes` is absent rather than continuing with a warning. However, string-marker matching remains fragile, does not roll back created handler files on failure, and does not detect duplicate route paths. Evaluated under findings M5 and m3.
- **D-m3 — `rume new` scaffolds a `Cargo.toml` with a local path dependency and no warning** — **REAFFIRMED**. Verified in `crates/cli/src/commands/new.rs:28–48, 126–137`. Project generation now defaults to a published `rume = "{version}"` dependency. When `--path` or `RUME_DEV_PATH` is passed, it injects the local path and prints an explicit post-scaffold note advising the user before publishing.
- **D-m4 — Generated code imports via `rume::axum` / `rume::serde`** — **REAFFIRMED**. Verified in `crates/cli/src/commands/generate/handler.rs:23–24`, `resource.rs:19–22`, and `rume-cli/templates/Cargo.toml.tpl:8–10`. Generated code now imports directly from `axum` and `serde`, and `Cargo.toml.tpl` declares direct dependencies on both.
- **D-m5 — No machine-readable output (`--json`) for `doctor`, `env check`, `config check`** — **REAFFIRMED** with new finding. Machine-readable JSON output was added to `doctor`, `env check`, and `config check` via `--format json`, and `list:routes` via `--json`. However, the flag syntax is inconsistent between inspection commands. Evaluated under finding m2.

---

## Findings

### M1 OpenAPI 3.1 generator discards route parameter matchers and infers schemas via parameter-name substring heuristics

- **Where:** `crates/core/src/openapi/spec.rs:163–177` and `crates/core/src/routing/router.rs:235–240`
- **What it says:**
  In `crates/core/src/openapi/spec.rs`:
  ```rust
  let mut parameters = Vec::new();
  for param in &route.params {
      parameters.push(OpenApiParameter {
          name: param.clone(),
          r#in: OpenApiParameterIn::Path,
          required: true,
          description: None,
          schema: if param.contains("uuid") {
              OpenApiSchema::uuid()
          } else if param.contains("id") || param.contains("count") {
              OpenApiSchema::integer()
          } else {
              OpenApiSchema::string()
          },
      });
  }
  ```
  In `crates/core/src/routing/router.rs`:
  ```rust
  self.registry.register(RouteInfo {
      method,
      path,
      name,
      params,
  });
  ```
- **Why it's a problem:**
  The OpenAPI generator does not inspect the route's declared parameter matchers (`Matcher::Number`, `Matcher::Uuid`, `Matcher::Slug`). In fact, `RouteSpec::build()` and `Router::route_spec` discard the declared `Matcher`s and store only `params: Vec<String>` in `RouteInfo`. The OpenAPI generator then guesses parameter types using crude substring matching on the parameter name:
  1. A route declared with `.where_param("id", Matcher::Uuid)` generates `{ "type": "integer", "format": "int64" }` because `"id"` contains `"id"` and not `"uuid"`. API client SDKs generated from this OpenAPI spec will attempt to pass integer IDs, breaking API integration.
  2. A route declared with `.where_param("code", Matcher::Number)` generates `{ "type": "string" }` because `"code"` does not contain `"id"` or `"count"`.
  3. Any parameter whose name contains `"id"` (e.g. `:identity`, `:identifier`, `:guide_id`, `:account_id`) is unconditionally typed as `integer`, even when the developer explicitly declared it as a UUID or slug.
  4. A parameter named `:uuid_id` is typed as `uuid` because `"uuid"` is checked before `"id"`.
  This violates OpenAPI schema extraction fidelity and produces incorrect API specifications and broken generated client code.
- **Suggested fix:**
  Update `RouteInfo` to store parameter metadata including the declared `Matcher` (e.g. `pub params: Vec<(String, Option<Matcher>)>` or `pub param_schemas: BTreeMap<String, OpenApiSchema>`). In `generate_openapi`, map parameter schemas directly from the declared `Matcher`:
  - `Matcher::Number` $\to$ `OpenApiSchema::integer()`
  - `Matcher::Uuid` $\to$ `OpenApiSchema::uuid()`
  - `Matcher::Slug` $\to$ `OpenApiSchema::string()`
  Fall back to `OpenApiSchema::string()` only when no explicit matcher was declared.

---

### M2 `AppContext::builder` fails to infer state generic and requires `S: Default`, forcing template and call sites to use 6 positional arguments

- **Where:** `crates/core/src/app/context.rs:85–95, 151–170` and `rume-cli/templates/src/app.rs.tpl:23–30`
- **What it says:**
  In `crates/core/src/app/context.rs`:
  ```rust
  impl<E, C, S> AppContext<E, C, S>
  where ...
  {
      pub fn builder(env: E, config: C) -> AppContextBuilder<E, C, S> {
          AppContextBuilder {
              env,
              config,
              ...
              state: None,
          }
      }
  }

  impl<E, C, S> AppContextBuilder<E, C, S>
  where ...
  {
      pub fn build(self) -> AppContext<E, C, S>
      where
          S: Default,
      {
          AppContext {
              ...
              state: self.state.unwrap_or_default(),
          }
      }
  }
  ```
  In `rume-cli/templates/src/app.rs.tpl`:
  ```rust
  #[derive(Clone)]
  pub struct AppState;
  ...
  let ctx = AppContext::new(
      env.clone(),
      config.clone(),
      paths,
      availability.clone(),
      health_checks,
      app_state,
  );
  ```
- **Why it's a problem:**
  1. **Generic inference failure on creation:** `AppContext::builder(env, config)` is implemented on `AppContext<E, C, S>`. Because `env` and `config` determine only `E` and `C`, the compiler cannot infer `S`. Calling `AppContext::builder(env, config).build()` fails to compile with `error[E0282]: type annotations needed`. Call sites are forced to supply explicit type parameters: `AppContext::<_, _, ()>::builder(env, config).build()`.
  2. **Unnecessary `where S: Default` restriction on `build()`:** Even if a caller provides state via `.state(my_state)`, `build()` still enforces `where S: Default`. Any application state that does not implement `Default` (e.g. database connection pools, Redis clients, HTTP client wrappers, or custom structs) cannot be built using `AppContextBuilder`.
  3. **Scaffold regression:** Because `AppState` in the CLI template does not implement `Default`, and because of the generic inference issue, `rume-cli/templates/src/app.rs.tpl` still scaffolds the fragile 6-positional-argument `AppContext::new(...)` constructor that finding D-M1 sought to eliminate.
- **Suggested fix:**
  1. Implement `AppContext::builder(env, config)` on `AppContext<E, C, ()>` returning `AppContextBuilder<E, C, ()>`:
     ```rust
     impl<E, C> AppContext<E, C, ()> {
         pub fn builder(env: E, config: C) -> AppContextBuilder<E, C, ()> { ... }
     }
     ```
  2. Provide a type-transitioning `.state<S2>(self, state: S2) -> AppContextBuilder<E, C, S2>` method that transitions the builder's state generic without requiring `S2: Default`.
  3. For `AppContextBuilder<E, C, ()>`, allow `.build() -> AppContext<E, C, ()>`. For `AppContextBuilder<E, C, S>`, allow `.build() -> AppContext<E, C, S>` whenever `state` was provided or when `S: Default`.
  4. Update `rume-cli/templates/src/app.rs.tpl` to use `AppContext::builder(env, config).state(app_state).build()`.

---

### M3 Configuration provenance reporting is entirely absent despite layered configuration support and `Cargo.toml` claims

- **Where:** `crates/core/src/config.rs:40–89`, `crates/core/Cargo.toml:22`, and `crates/cli/src/commands/config/`
- **What it says:**
  In `crates/core/Cargo.toml`:
  ```toml
  # Layered configuration with provenance.
  ```
  In `crates/core/src/config.rs`:
  ```rust
  fn build_figment(dir: &Path) -> Result<Figment> {
      let env_name = crate::env::resolve_env_name();
      let candidates = [
          dir.join("config").join("app.toml"),
          dir.join("config").join(format!("app.{env_name}.toml")),
          dir.join("config").join("app.local.toml"),
          dir.join("config").join(format!("app.{env_name}.local.toml")),
      ];
      let mut figment = Figment::new();
      for path in candidates {
          if path.is_file() {
              figment = figment.merge(Toml::file(&path));
          }
      }
      ...
  }
  ```
- **Why it's a problem:**
  While environment loading (`crates/core/src/env/loading.rs`) explicitly tracks provenance via `ResolvedEnv.sources: BTreeMap<String, String>` and CLI `rume env explain <key>`, configuration loading (`crates/core/src/config.rs`) provides no provenance tracking whatsoever:
  1. `config::load<T>()` and `config::load_raw()` return opaque values with no metadata recording which configuration file (`app.toml`, `app.development.toml`, `app.local.toml`, etc.) supplied each key.
  2. The CLI provides `rume config check` (printing the merged TOML) but has no `rume config explain <key>` command to diagnose which candidate file took precedence for a given configuration setting.
  3. The whole-document review charter explicitly mandates "configuration provenance reporting." Developers troubleshooting configuration overrides in staging and production environments have no framework tooling to inspect configuration provenance.
- **Suggested fix:**
  1. Introduce a `ResolvedConfig` or `ConfigSourceMap` struct that records key-level file provenance during the layered candidate merge.
  2. Expose `config::load_with_provenance(dir: &Path)` returning both the merged configuration and the source map.
  3. Add `rume config explain <key>` to `rume-cli`, mirroring `rume env explain <key>`, to display the resolved value, winning source file, and candidate precedence.

---

### M4 Environment interpolation error formatting hides the variable name by escaping format braces (`${{variable}}`)

- **Where:** `crates/core/src/env/error.rs:23–27`
- **What it says:**
  ```rust
  #[error("failed to interpolate ${{variable}}: variable not found")]
  Interpolation { variable: String },

  #[error("circular reference detected while interpolating ${{variable}}")]
  CircularReference { variable: String },

  #[error("maximum interpolation depth ({depth}) exceeded while interpolating ${{{variable}}}")]
  MaxDepthExceeded { variable: String, depth: usize },
  ```
- **Why it's a problem:**
  In Rust format strings (and `thiserror`), `{{` and `}}` are escape sequences that output literal `{` and `}` characters.
  Because lines 23 and 26 write `"${{variable}}"`, the identifier `variable` is treated as literal text inside the escaped braces. The `variable: String` field is **never interpolated**.
  When an environment variable interpolation fails at boot:
  - Line 23 literally formats as: `"failed to interpolate ${variable}: variable not found"`
  - Line 26 literally formats as: `"circular reference detected while interpolating ${variable}"`
  The developer is told that `${variable}` failed, but the actual failing variable name (e.g. `DATABASE_URL` or `API_KEY`) is completely suppressed. Line 29 correctly used `${{{variable}}}` (`${` + `{variable}` + `}`), demonstrating that lines 23 and 26 are defective format strings.
- **Suggested fix:**
  Fix the format string on lines 23 and 26 to use triple braces:
  ```rust
  #[error("failed to interpolate ${{{variable}}}: variable not found")]
  Interpolation { variable: String },

  #[error("circular reference detected while interpolating ${{{variable}}")]
  CircularReference { variable: String },
  ```

---

### M5 Scaffolding template README advertises non-existent `/health` route, while `routes.rs.tpl` squats `/users` and `/posts` with dummy health handlers

- **Where:** `rume-cli/templates/README.md.tpl:11`, `rume-cli/templates/src/routes.rs.tpl:10–14`, and `rume-cli/templates/src/app.rs.tpl:32–34`
- **What it says:**
  In `rume-cli/templates/README.md.tpl`:
  ```markdown
  ## Run
  cargo run

  Then visit http://127.0.0.1:3333/health.
  ```
  In `rume-cli/templates/src/routes.rs.tpl`:
  ```rust
  pub fn router(_ctx: AppContext<Env, Config, AppState>) -> Router<AppContext<Env, Config, AppState>> {
      let mut router = Router::<AppContext<Env, Config, AppState>>::new();
      router = router.get("/users", crate::handlers::health::health);
      router = router.get("/posts", crate::handlers::health::health);
      // rume:routes
      router
  }
  ```
  In `rume-cli/templates/src/app.rs.tpl`:
  ```rust
  let router = routes::router(ctx.clone());
  let router = health_routes::<Env, Config, AppState>().merge(router);
  ```
- **Why it's a problem:**
  1. **Immediate 404 on quickstart:** When a newcomer follows the quickstart instructions in `README.md` (`rume new myapp` $\to$ `cargo run` $\to$ visit `http://127.0.0.1:3333/health`), the server responds with **404 Not Found**. The route `/health` is never registered: `health_routes()` registers `/health/live` and `/health/ready`, and `routes.rs.tpl` mounts the health handler on `/users` and `/posts`.
  2. **Route squatting and generator collision:** In `routes.rs.tpl`, `health::health` is bizarrely mounted at `/users` and `/posts`. If the user follows the standard tutorial or CLI help and runs `rume generate handler users` or `rume generate resource users`, the generator inserts `router = router.get("/users", crate::handlers::users::index)` into `src/routes.rs`. Axum then panics at startup with a route collision error (`Cannot merge two routers that both define a route for: /users`), breaking the newly scaffolded project.
- **Suggested fix:**
  In `rume-cli/templates/src/routes.rs.tpl`, mount the custom health handler at `/health`:
  ```rust
  let mut router = Router::<AppContext<Env, Config, AppState>>::new();
  router = router.get("/health", crate::handlers::health::health);
  // rume:routes
  router
  ```
  Remove the dummy `/users` and `/posts` routes from the starter template so `rume generate handler users` and `rume generate resource users` can run cleanly without duplicate route conflicts.

---

### m1 Orphaned `ops_router` in `shutdown.rs` preserves duplicate health check API with conflicting state model

- **Where:** `crates/core/src/app/shutdown.rs:58–88` and `crates/core/src/prelude.rs:7`
- **What it says:**
  ```rust
  pub fn ops_router(router: Router<Availability>) -> Router<Availability> {
      router
          .route("/ops/live", get(move |State(av): State<Availability>| async move { ... }))
          .route("/ops/ready", get(move |State(av): State<Availability>| async move { ... }))
  }
  ```
- **Why it's a problem:**
  In v01 finding D-M3, two duplicate health check APIs were identified (`health_routes` and `ops_router`). In v02, `ops_router` was renamed from `/health/*` to `/ops/*` and placed in `app/shutdown.rs`, but the duplication remains:
  - `rume::routing::health_routes()` operates on `Router<AppContext<E, C, S>>` at `/health/live` and `/health/ready`, running registered `HealthChecks`.
  - `rume::app::ops_router()` operates on `Router<Availability>` at `/ops/live` and `/ops/ready`, ignoring `HealthChecks`.
  `ops_router` is located in `shutdown.rs` despite having nothing to do with graceful shutdown, is completely unused across all templates and integration tests, and is exported in `prelude.rs`, confusing developers about which endpoints to configure for infrastructure liveness/readiness probes.
- **Suggested fix:**
  Deprecate or remove `ops_router` from `app/shutdown.rs` and `prelude.rs`. If a standalone ops router without `AppContext` is required for minimal applications, consolidate it into `crates/core/src/routing/health.rs` under a unified naming convention.

---

### m2 Inconsistent JSON CLI flags across inspection commands (`--format json` vs `--json`)

- **Where:** `crates/cli/src/cli.rs:40–51, 67–98`
- **What it says:**
  `rume list:routes` declares `#[arg(long)] json: bool`.
  `rume doctor`, `rume env check`, and `rume config check` declare `#[arg(long, default_value = "human")] format: String`.
- **Why it's a problem:**
  The CLI exhibits flag inconsistency:
  - `rume doctor --json` fails with `error: unexpected argument '--json' found`.
  - `rume list:routes --format json` fails with `error: unexpected argument '--format' found`.
  A user scripting Rume in CI or automation must remember which commands require `--json` and which require `--format json`.
- **Suggested fix:**
  Support both `--json` and `--format json` across all inspection commands (`doctor`, `env check`, `config check`, and `list:routes`), or standardize all commands on `--format <human|json>` with `--json` as an alias.

---

### m3 CLI generators use naive first-letter `capitalize` rather than `rume::helpers::pascal_case`, generating non-idiomatic struct names

- **Where:** `crates/cli/src/commands/generate/resource.rs:192–198, 224`, `handler.rs:100–106`, `model.rs:57–63`, `service.rs:57–63`
- **What it says:**
  ```rust
  fn capitalize(value: &str) -> String {
      let mut chars = value.chars();
      match chars.next() {
          Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
          None => String::new(),
      }
  }
  ```
- **Why it's a problem:**
  When generating components with hyphenated or snake_case names (e.g. `rume generate model user-profile` or `rume generate resource order-item`), `module_name` becomes `user_profile` and `capitalize(&module_name)` yields `User_profile`.
  The generated code contains:
  `pub struct User_profile` and `pub struct Order_itemCreateRequest`.
  Struct names containing internal underscores trigger rustc `non_camel_case_types` compiler warnings. The core crate already provides `rume::helpers::case::pascal_case`, which properly converts `"user-profile"` and `"user_profile"` into `"UserProfile"`.
- **Suggested fix:**
  Replace the ad-hoc `capitalize` function across all generators in `rume-cli` with `rume::helpers::case::pascal_case`.

---

### m4 `RouteGroup` lacks fluent `.layer()` middleware mounting method

- **Where:** `crates/core/src/routing/group.rs:21–156`
- **What it says:**
  `RouteGroup<S>` provides `.prefix()`, `.get()`, `.post()`, `.put()`, `.patch()`, `.delete()`, `.add()`, `.route_spec()`, and `.resource()`, but provides no `.layer<L>(self, layer: L)` method.
- **Why it's a problem:**
  In modern web frameworks, the primary reason to group routes is to apply shared configuration: a shared URL prefix and shared middleware (such as authentication, CSRF validation, or rate limiting). Because `RouteGroup` lacks `.layer()`, developers cannot apply middleware to a group directly. Instead, they must call `.build()`, apply `.layer()` to the resulting `Router`, and manually merge it back.
- **Suggested fix:**
  Add a fluent `.layer<L>(mut self, layer: L) -> Self` method to `RouteGroup<S>`:
  ```rust
  pub fn layer<L>(mut self, layer: L) -> Self
  where
      L: tower_layer::Layer<axum::routing::Route> + Clone + Send + 'static,
      L::Service: tower_service::Service<axum::http::Request<axum::body::Body>> + Clone + Send + 'static,
      ...
  {
      self.router = self.router.layer(layer);
      self
  }
  ```

---

### m5 `RumeBuilder` lacks fluent `.openapi()` and `.metrics()` methods available on `Router`

- **Where:** `crates/core/src/app/builder.rs:240–250, 480–550` and `crates/core/src/routing/router.rs:374–402`
- **What it says:**
  `rume::routing::Router` provides fluent helpers:
  ```rust
  pub fn openapi(self, path: &str, info: crate::openapi::OpenApiInfo) -> Self
  pub fn metrics(self, path: &str, registry: crate::metrics::MetricsRegistry) -> Self
  ```
  `RumeBuilder` mirrors all standard router methods (`.get()`, `.post()`, `.put()`, `.patch()`, `.delete()`, `.route()`, `.route_spec()`, `.resource()`), but omits `.openapi()` and `.metrics()`.
- **Why it's a problem:**
  Developers using `Rume::new()` or `Rume::builder(ctx)` who wish to expose OpenAPI documentation or Prometheus metrics cannot chain `.openapi("/openapi.json", info)` or `.metrics("/metrics", registry)` directly on the application builder. They are forced to construct an intermediate `rume::routing::Router` and pass it via `.router()`, breaking builder fluency.
- **Suggested fix:**
  Forward `.openapi(path, info)` and `.metrics(path, registry)` methods on `RumeBuilder` to its internal `self.router`.

---

### m6 Stale "extractor" docstrings and unused phantom type parameter on `ValidatedParam`

- **Where:** `crates/core/src/routing/validated_param.rs:1–4, 35–39, 61–64` and `crates/core/src/prelude.rs:60`
- **What it says:**
  ```rust
  //! Validated parameter extraction.
  //!
  //! Provides [`ValidatedParam`], which validates a route parameter value
  //! against a [`Matcher`] and returns the validated string.
  ...
  pub struct ValidatedParam<M> { ... }
  ...
  /// Consume the extractor and return the validated string value.
  pub fn into_inner(self) -> String {
  ```
- **Why it's a problem:**
  In v01 finding C1, `ValidatedParam`'s broken `FromRequestParts<()>` implementation was removed, moving route parameter validation to `ParamValidationLayer` on `RouteSpec` and `ValidatedPath<T>` in `crates/core/src/validation.rs`. However, `ValidatedParam` retains stale docstrings calling it an "extractor", retains a phantom marker `M` that is completely disconnected from the runtime `matcher: Matcher` argument in `from_value()`, and is re-exported in `prelude.rs`. Attempting to use `ValidatedParam` as a handler extractor fails compilation, misleading developers.
- **Suggested fix:**
  Remove the unused phantom generic `M` from `ValidatedParam`, update documentation to clarify that it is a manual validation utility, direct developers to `RouteSpec::where_param` and `ValidatedPath` for Axum handler extraction, and remove `ValidatedParam` from `rume::prelude`.

---

### m7 Generated project `tests/smoke.rs.tpl` tests a throwaway inline router rather than the scaffolded application

- **Where:** `rume-cli/templates/tests/smoke.rs.tpl:24–44`
- **What it says:**
  ```rust
  #[tokio::test]
  async fn health_endpoint_returns_ok() {
      use axum::{Json, Router, routing::get};
      ...
      let router = Router::new().route("/health", get(health));
      let app = TestApp::new(router);
      let response = app.get("/health").await;
      assert_eq!(response.status(), axum::http::StatusCode::OK);
  }
  ```
- **Why it's a problem:**
  The generated smoke test creates an ad-hoc inline Axum router with an inline dummy handler function rather than exercising the project's actual routes in `crate::routes::router` or `health_routes()`. When a user runs `cargo test` in a newly scaffolded project, the test passes 100% of the time even if the application's actual routes, handlers, or configurations are completely broken.
- **Suggested fix:**
  Update `smoke.rs.tpl` to test the actual application router or health routes with `TestApp`:
  ```rust
  let ctx = AppContext::builder(env, config).build();
  let router = crate::routes::router(ctx.clone());
  let app = TestApp::new(router);
  ```

---

### m8 Terminal UI `Sticker` calculates box padding using byte length instead of display width

- **Where:** `crates/cli/src/ui/sticker.rs:25–32`
- **What it says:**
  ```rust
  let max_len = self.lines.iter().map(|l| l.len()).max().unwrap_or(0);
  let border = "─".repeat(max_len + 2);
  println!("┌{}┐", border);
  for line in &self.lines {
      let padding = " ".repeat(max_len - line.len());
      println!("│ {}{} │", line, padding);
  }
  ```
- **Why it's a problem:**
  `line.len()` computes byte length rather than Unicode terminal display width. If a sticker line contains multi-byte characters (e.g. emojis, accents, non-ASCII project names) or ANSI escape styling, `max_len - line.len()` produces incorrect padding, causing the right-hand border (`│`) of the sticker box to become misaligned and jagged.
- **Suggested fix:**
  Compute line width using character count (`line.chars().count()`) or terminal display width via `console::measure_text_width(line)`.

---

## Positive notes

- **Portability by Default (`rume new`)**: Addressing v01 finding D-m3, `rume new` scaffolds clean, portable `Cargo.toml` manifests with published version pins by default. Local framework path dependencies are only enabled when `--path` or `RUME_DEV_PATH` is explicitly provided, accompanied by clear user-facing guidance.
- **Direct Scaffolding Dependencies (`D-m4`)**: Scaffolding templates and CLI generators now generate direct imports from `axum` and `serde`, and generated projects declare direct dependencies, completely eliminating brittle third-party re-export patterns.
- **Zero Secret Leakage in CLI Inspection**: Addressing v01 finding S-M3, `rume env check` and `rume config check` redact secret keys and credential URLs by default in both human-readable and machine-readable JSON modes, requiring an explicit `--unredacted` flag for plaintext output.
- **Comprehensive, Pure Helpers (`rume::helpers`)**: The pure helper suite (`case`, `slug`, `mask`, `safe_equal`, `truncate`, `whitespace`, `bytes`, `duration`, `sentence`, `ordinal`) is impeccably implemented with `#![forbid(unsafe_code)]`, zero-allocation `Cow` fast paths, constant-time `safe_equal` hardening with `black_box`, and UTF-8 boundary safety.
- **Seamless Typed Validation (`rume::validation`)**: Extractors `ValidatedJson`, `ValidatedPath`, `ValidatedQuery`, and `ValidatedForm` provide elegant, declarative validation powered by `garde::Validate`, automatically mapping validation failures into RFC 9457 422 Problem Details.
- **Progressive Composition Ergonomics**: `Rume::new()` provides an authentic, intuitive 3-line entry point (`Rume::new().get("/", handler).run().await`) that starts with sensible defaults and gracefully scales to modules and typed contexts.
- **High-Quality CLI Feedback**: The CLI provides informative stickers, clear next-step onboarding instructions (`cd`, `cargo run`, `rume doctor`), interactive prompts when arguments are omitted, and structured table rendering.

---

## Verdict

**Not ready to proceed without a fix pass.**

While the tooling foundation is substantially matured over v01, five Major findings create immediate friction, broken quickstart onboarding, and invalid API contracts:

1. **M1** — Restore OpenAPI extraction fidelity by retaining declared parameter `Matcher`s in route metadata and mapping schemas directly from matchers rather than crude parameter-name substring heuristics.
2. **M2** — Fix `AppContext::builder` generic inference and eliminate the `where S: Default` restriction, updating `app.rs.tpl` to retire the 6-positional-arg constructor.
3. **M3** — Introduce configuration provenance tracking and CLI reporting (`rume config explain`), fulfilling the advertised layered configuration contract.
4. **M4** — Fix the escaped format string on `EnvError::Interpolation` and `CircularReference` (`${{{variable}}}`) so that failing environment variable names are visible in error messages.
5. **M5** — Align `README.md.tpl` and `routes.rs.tpl` so the quickstart `/health` endpoint resolves, and remove dummy route squatting at `/users` and `/posts` to prevent generator collisions.

Resolving findings M1–M5 alongside polish items m1–m8 will give Rume an exceptionally polished, robust, and delightful developer experience.
