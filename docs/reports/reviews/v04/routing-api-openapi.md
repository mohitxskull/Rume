# Routing, Validation, Errors & OpenAPI Review — v04

## Summary

This lens conducted a comprehensive whole-document audit of Rume's HTTP routing layer, parameter matching, health check subsystem, RFC 9457 error contracts, request validation extractors, Prometheus metrics pipeline, and OpenAPI 3.1 specification generation. While the foundation exhibits exceptional discipline in RFC 9457 Problem Details modeling, secret redaction, and bounded-cardinality metrics tracking, **one Critical** and **three Major** defects prevent production certification. Most critically, `/health/ready` employs negative blacklist checking that causes it to report `200 OK` during application boot before startup hooks complete, prematurely routing production traffic to unready nodes.

---

## Prior decisions

> [!NOTE]
> No v03 review lens covered routing, validation, error contracts, or OpenAPI generation. The prior decision baseline is derived from `docs/reports/reviews/v01/correctness.md`, `docs/reports/reviews/v02/correctness.md`, `docs/reports/reviews/v02/dx-tooling.md`, and `docs/reports/reviews/v02/testing.md`.

- **v01 C1 (`ValidatedParam<M>` cannot compile with `AppContext` state)** — **REAFFIRMED**. Verified in `crates/core/src/routing/spec.rs:166–302` and `crates/core/src/routing/validated_param.rs:1–64`. HTTP parameter validation was decoupled from handler-level state extraction and moved to `ParamValidationLayer` applied to `MethodRouter` on `RouteSpec`. `ValidatedParam` was refactored into a value container populated via `ValidatedParam::from_value`, and typed parameter extraction is provided by `ValidatedPath<T>` in `crates/core/src/validation.rs:161–184`. Handlers compile with any state.
- **v01 m1 (`Matcher::Number` doc "digits only" does not match `u64`-bounded parsing)** — **REAFFIRMED**. Verified in `crates/core/src/routing/matchers.rs:15–17, 24–25`. The documentation and inline comments explicitly specify that the value must parse as `u64`, and overflowing digit strings are rejected.
- **v01 m2 (`RouteGroup::add` does not normalize path separators)** — **REAFFIRMED**. Verified in `crates/core/src/routing/registry.rs:137–158` and `crates/core/src/routing/group.rs:42, 53, 64, 75, 86, 93, 104, 126, 162`. `join_path` normalizes adjacent and missing slashes across all route mounting methods.
- **v01 m3 (`RouteSpec::build()` registers in global registry even when unmounted)** — **REAFFIRMED**. Verified in `crates/core/src/routing/spec.rs:162–164` and `crates/core/src/routing/router.rs:221–252`. Global singleton `ROUTE_REGISTRY` was eliminated; `RouteRegistry` is application-scoped, `RouteSpec::build()` is pure and side-effect free, and route registration occurs only upon explicit router mounting.
- **v01 m4 (`health_routes` `/health/ready` returns 503 for `Warning`, not just failures)** — **REAFFIRMED**. Verified in `crates/core/src/routing/health.rs:141–143, 238–251, 618–626`. The architectural decision that readiness requires full health (`is_healthy == false` on `Warning`) was maintained, and documentation and tests explicitly affirm that degraded warning status yields HTTP 503.
- **v02 m1 (`ValidatedParam` documentation refers to "extractor", but `FromRequestParts` is no longer implemented)** — **REAFFIRMED**. Verified in `crates/core/src/routing/validated_param.rs:6–8`. Documentation clarifies that `ValidatedParam` is a validated value container populated via `from_value`, not an Axum request extractor.
- **v02 DX M1 (OpenAPI 3.1 generator discards route parameter matchers and infers schemas via parameter-name substring heuristics)** — **REAFFIRMED**. Verified in `crates/core/src/routing/registry.rs:40`, `crates/core/src/routing/router.rs:230, 250`, and `crates/core/src/openapi/spec.rs:164–174`. Route metadata now retains declared `matchers: Vec<(String, Matcher)>`, and `generate_openapi` directly inspects matchers to generate schema types (`Matcher::Number` $\to$ integer, `Matcher::Uuid` $\to$ uuid format, `Matcher::Slug` $\to$ regex string).
- **v02 DX m4 (`RouteGroup` lacks fluent `.layer()` middleware mounting method)** — **REAFFIRMED**. Verified in `crates/core/src/routing/group.rs:142–154`. `RouteGroup::layer` applies Tower layers group-wide.
- **v02 DX m5 / Architecture m5 (`Router` lacks fluent `.openapi()` and `.metrics()` methods)** — **REAFFIRMED**. Verified in `crates/core/src/routing/router.rs:362–383`. Both endpoints can be mounted fluently on `Router`.
- **v02 Testing / insta snapshot remediation (OpenAPI and Problem Details snapshot tests)** — **REAFFIRMED**. Verified in `crates/core/tests/integration/openapi.rs:30–86` and `crates/core/tests/integration/error.rs:62–72, 91–101`. `insta::assert_json_snapshot!` is integrated into integration test suites.

---

## Findings

### C1 Readiness probe `/health/ready` returns 200 OK during startup (`AvailabilityState::Live`), routing traffic before modules and startup hooks are ready

- **Where:** `crates/core/src/routing/health.rs:327–358` and `crates/core/src/app/builder.rs:1261–1342`
- **What it says:**
  In `crates/core/src/routing/health.rs`:
  ```rust
  .get(
      "/health/ready",
      move |State(ctx): State<AppContext<E, C, S>>| async move {
          // S-m4: readiness also respects lifecycle state — refuse
          // traffic when the application is terminating or
          // refusing traffic, even if all health checks pass.
          if ctx.availability.state() == AvailabilityState::RefusingTraffic
              || ctx.availability.state() == AvailabilityState::Unknown
              || ctx.availability.state() == AvailabilityState::Broken
          {
              return (
                  StatusCode::SERVICE_UNAVAILABLE,
                  Json(json!({"status": "error"})),
              )
                  .into_response();
          }
          let report = ctx.health_checks.run().await;
          ...
          let code = if report.is_healthy {
              StatusCode::OK
          } else {
              StatusCode::SERVICE_UNAVAILABLE
          };
          (code, Json(json!({"status": status}))).into_response()
      },
  )
  ```
  In `crates/core/src/app/builder.rs`:
  ```rust
  // Step 3: Booted & Live
  serve_ctx.lifecycle.set(LifecycleState::Booted);
  serve_ctx.availability.set(AvailabilityState::Live);

  // Step 4: Module startup hooks in topological dependency order (migrations, pool init)
  for m in startup_modules { ... m.on_start(&mod_ctx).await ... }

  // Step 5: Application startup hooks with cleanup on failure (cache warming, on_ready)
  for hook in on_ready { ... hook(serve_ctx.clone()).await ... }

  // Step 6: Declare Ready
  serve_ctx.lifecycle.set(LifecycleState::Ready);
  serve_ctx.availability.set(AvailabilityState::Ready);
  ```
- **Why it's a problem:**
  In containerized environments (Kubernetes, AWS ECS, Nomad), orchestration engines query `/health/ready` to determine when an instance is ready to receive user traffic.
  During application startup, `run_with_bound_listener` binds the TCP socket and sets `AvailabilityState::Live` at Step 3. Steps 4 and 5 (running database migrations, warming in-memory caches, acquiring leader locks, executing user `on_ready` hooks) can take seconds or tens of seconds.
  `/health/ready` evaluates readiness using a **negative blacklist**: it only refuses traffic if `availability` is `RefusingTraffic`, `Unknown`, or `Broken`. When the application is in `AvailabilityState::Live` (or `AvailabilityState::Created` / `Starting`), this check evaluates to `false`. `/health/ready` proceeds to run `ctx.health_checks.run().await`. If dependencies (e.g. Postgres, Redis) are reachable, `report.is_healthy` is `true`, and `/health/ready` returns **HTTP 200 OK `{"status": "ok"}`**.
  Kubernetes immediately routes incoming client traffic to the pod before database migrations finish or caches are warm, causing request failures, uninitialized state panics, and data corruption.
- **Suggested fix:**
  Enforce positive readiness gating in `/health/ready`:
  ```rust
  if ctx.availability.state() != AvailabilityState::Ready {
      return (
          StatusCode::SERVICE_UNAVAILABLE,
          Json(json!({"status": "error"})),
      )
          .into_response();
  }
  ```
  An application must never claim readiness until its state is affirmatively `AvailabilityState::Ready`.

---

### M1 Eager OpenAPI generation in `Router::openapi()` captures static snapshot at registration time, discarding all subsequent routes and merged subrouters

- **Where:** `crates/core/src/routing/router.rs:361–369` (corresponds with Lens 7 finding M1 on `RumeBuilder::openapi`)
- **What it says:**
  ```rust
  /// Mounts an OpenAPI JSON endpoint at the specified path (e.g. `/openapi.json`).
  pub fn openapi(self, path: &str, info: crate::openapi::OpenApiInfo) -> Self {
      let spec = self.openapi_spec(info);
      let json_value = spec.to_json_value();
      self.get(path, move || {
          let val = json_value.clone();
          async move { axum::Json(val) }
      })
  }
  ```
- **Why it's a problem:**
  `Router::openapi()` executes `self.openapi_spec(info)` and `spec.to_json_value()` eagerly at builder definition time, capturing a static JSON value inside the handler closure.
  Consequently:
  1. Any route declared after `.openapi(...)` (e.g. `Router::new().openapi("/openapi.json", info).get("/users", list_users)`) is completely absent from the rendered `/openapi.json` spec.
  2. Any module router or subrouter merged after `.openapi(...)` via `.merge(...)` or `.merge_stateless(...)` is omitted.
  3. This mirrors the design flaw identified in `RumeBuilder::openapi()` (Lens 7 M1), breaking OpenAPI documentation for modular applications.
- **Suggested fix:**
  Generate the OpenAPI specification lazily on request, or wrap the application-scoped `RouteRegistry` in an `Arc<RwLock<RouteRegistry>>` / state reader:
  ```rust
  pub fn openapi(self, path: &str, info: crate::openapi::OpenApiInfo) -> Self {
      let registry = self.registry.clone();
      self.get(path, move || {
          let reg = registry.clone();
          let inf = info.clone();
          async move { axum::Json(reg.to_openapi(inf).to_json_value()) }
      })
  }
  ```
  (Or better, read `ctx.route_registry` from Axum `State` if state `S` is bound).

---

### M2 OpenAPI parameter schema regex pattern for `Matcher::Slug` contradicts `Matcher::Slug::validate` runtime validation

- **Where:** `crates/core/src/openapi/spec.rs:169–173` vs `crates/core/src/routing/matchers.rs:66–85`
- **What it says:**
  In `crates/core/src/openapi/spec.rs`:
  ```rust
  crate::routing::Matcher::Slug => OpenApiSchema {
      r#type: "string".into(),
      format: None,
      pattern: Some("^[a-z0-9]+(?:-[a-z0-9]+)*$".into()),
  },
  ```
  In `crates/core/src/routing/matchers.rs`:
  ```rust
  fn is_valid_slug(value: &str) -> bool {
      if value.len() < 3 {
          return false;
      }
      if value.contains(' ') {
          return false;
      }
      if value.starts_with('-') || value.starts_with('_') {
          return false;
      }
      if value.ends_with('-') || value.ends_with('_') {
          return false;
      }
      if value.contains("--") || value.contains("__") {
          return false;
      }
      value
          .chars()
          .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
  }
  ```
- **Why it's a problem:**
  The schema published in OpenAPI 3.1 directly contradicts the runtime parameter validation in both directions:
  1. **Underscores & uppercase rejected by OpenAPI, accepted by server:** `is_valid_slug` explicitly permits underscores `_` and uppercase letters (`c.is_ascii_alphanumeric() || c == '-' || c == '_'`), so `user_profile` or `API-Docs` are valid slugs at runtime. The OpenAPI pattern `^[a-z0-9]+(?:-[a-z0-9]+)*$` strictly forbids underscores and uppercase letters. Generated SDKs and API gateways (Kong, Envoy) validating requests against the OpenAPI spec will reject valid client requests before they reach the server.
  2. **Short slugs accepted by OpenAPI, rejected by server:** `is_valid_slug` enforces `value.len() < 3 => false`, rejecting `"ab"` or `"v1"`. The OpenAPI pattern `^[a-z0-9]+(?:-[a-z0-9]+)*$` matches 1- and 2-character strings, allowing invalid parameters through client-side validation only to fail server-side with 400 Bad Request.
- **Suggested fix:**
  Harmonize `is_valid_slug` and the OpenAPI schema pattern. Either restrict slugs canonical to lowercase alphanumeric and hyphens with minimum length 3:
  ```rust
  pattern: Some("^[a-z0-9]{3,}(?:-[a-z0-9]+)*$".into())
  ```
  and update `is_valid_slug` accordingly, or update the OpenAPI schema pattern to match the multi-character, underscore-allowing slug grammar:
  ```rust
  pattern: Some("^[a-zA-Z0-9][a-zA-Z0-9_-]{1,}[a-zA-Z0-9]$".into())
  ```

---

### M3 Health check probe race on cache expiry causes thundering herd stampede and serial lock churn

- **Where:** `crates/core/src/routing/health.rs:147–176, 178–221`
- **What it says:**
  ```rust
  if let Some(ttl) = self.cache_ttl {
      let now = Instant::now();
      let cache = self.cache.read().unwrap();
      let all_cached = !self.checks.is_empty()
          && self.checks.iter().all(|check| {
              cache
                  .get(check.name())
                  .is_some_and(|(instant, _)| now.duration_since(*instant) < ttl)
          });
      drop(cache);
      if all_cached {
          let checks = self
              .checks
              .iter()
              .map(|check| {
                  let mut result = self
                      .cache
                      .read()
                      .unwrap()
                      .get(check.name())
                      .expect("cached result present")
                      .1
                      .clone();
                  result.cached = true;
                  result
              })
              .collect::<Vec<_>>();
          return Self::build_report(checks);
      }
  }

  // Run all checks concurrently, each wrapped in a per-check timeout.
  let mut handles = Vec::with_capacity(self.checks.len());
  for check in &self.checks {
      ... tokio::spawn(async move { ... check.run().await ... })
  }
  ```
- **Why it's a problem:**
  1. **Lock churn:** When all checks are cached, line 156 drops `cache`. Then lines 162–172 reacquire `self.cache.read().unwrap()` inside an iterator `map` closure $N$ times in a row, once for every registered check, instead of holding the read guard once.
  2. **Probe-race / cache stampede:** When `cache_ttl` expires, or on server startup under concurrent probes (e.g. Kubernetes readiness probes arriving simultaneously from multiple nodes), all concurrent requests observe `all_cached == false`. None of the requests await an existing in-flight run. Every incoming request spawns a separate batch of background tasks executing all registered health checks (hitting database, Redis, S3, external services) concurrently. When they complete, each request contends on `self.cache.write().unwrap()` to overwrite the cache with identical results. Under high traffic, this thundering herd causes connection exhaustion on downstream databases.
- **Suggested fix:**
  1. In the cache-hit branch, hold the read lock guard once and clone the entries in a single pass without dropping and reacquiring $N$ times.
  2. Introduce in-flight coalescing (single-flight execution via `tokio::sync::Mutex` or `watch` channel) so that when a check run is in-flight, concurrent probe requests await the in-flight execution rather than spawning duplicate check tasks.

---

### m1 `/health/ready` endpoint collapses `Warning` status into `"error"` in response body while returning 503

- **Where:** `crates/core/src/routing/health.rs:350–356`
- **What it says:**
  ```rust
  let status = if report.is_healthy { "ok" } else { "error" };
  let code = if report.is_healthy {
      StatusCode::OK
  } else {
      StatusCode::SERVICE_UNAVAILABLE
  };
  (code, Json(json!({"status": status}))).into_response()
  ```
- **Why it's a problem:**
  `HealthReport::build_report` (lines 243–248) explicitly sets `status = "warning"` when checks report `HealthStatus::Warning` with no failed checks.
  However, `/health/ready` ignores `report.status` and computes `status = if report.is_healthy { "ok" } else { "error" }`.
  Because `is_healthy` is `false` for both `Warning` and `Failed`, the response body is always rendered as `{"status": "error"}`.
  A monitoring agent or operator cannot distinguish whether the 503 response was caused by a complete outage (`"error"`) or an operable but degraded dependency (`"warning"`), discarding the information provided by `HealthStatus::Warning`.
- **Suggested fix:**
  Use the report's computed status string in the response:
  ```rust
  let status = if report.is_healthy { "ok" } else { &report.status };
  ```

---

### m2 `MetricsLayer` hardcodes bypass for exact path `"/metrics"`, tracking scraper requests when metrics endpoint is mounted on custom paths

- **Where:** `crates/core/src/metrics/layer.rs:67–70`
- **What it says:**
  ```rust
  if req.uri().path() == "/metrics" {
      let mut inner = self.inner.clone();
      return Box::pin(async move { inner.call(req).await });
  }
  ```
- **Why it's a problem:**
  `Router::metrics(path, registry)` and `RumeBuilder::metrics(path, registry)` allow binding Prometheus scraping to any custom path (e.g. `"/admin/metrics"`, `"/ops/metrics"`, `"/internal/prometheus"`).
  However, `MetricsLayer` hardcodes `req.uri().path() == "/metrics"`. If an application exposes metrics on any path other than `"/metrics"`, the scraper's own requests are recorded in `http_requests_total` and `http_request_duration_seconds`, corrupting traffic counts and latency histograms.
- **Suggested fix:**
  Provide `MetricsLayer::new_with_path(registry, path)` or record the metrics path on `MetricsRegistry` so the layer bypasses the actual configured endpoint.

---

### m3 Route parameter matcher declaration does not validate parameter existence against path template

- **Where:** `crates/core/src/routing/spec.rs:101–104, 268–278` and `crates/core/src/routing/router.rs:221–252`
- **What it says:**
  In `crates/core/src/routing/spec.rs`:
  ```rust
  for (name, matcher) in &*matchers {
      let value = match params.get(name) {
          Some(v) => v,
          None => {
              return Ok(AppError::BadRequest {
                  code: "MISSING_PARAM",
                  message: format!("missing parameter: {name}"),
              }
              .into_response());
          }
      };
      ...
  }
  ```
- **Why it's a problem:**
  If a developer declares a route matcher with a typo in the parameter name:
  ```rust
  RouteSpec::get("/users/{user_id}")
      .where_param("id", Matcher::Number)
  ```
  `RouteSpec::build()` and `Router::route_spec()` accept it silently without compile-time, build-time, or mount-time error.
  At runtime, Axum extracts `user_id`, so `params.get("id")` is permanently `None`. Every incoming HTTP request to `/users/123` fails closed with 400 Bad Request (`MISSING_PARAM: missing parameter: id`), making the route completely broken in production with no preflight warning.
- **Suggested fix:**
  In `RouteSpec::build()` or `Router::route_spec()`, validate that every parameter name in `self.params` is contained in `self.param_names()`. If an unmatched parameter is declared, fail preflight or panic with an actionable error message naming the mismatched parameter.

---

### m4 OpenAPI 3.1 generator lacks Request Body, Security Scheme, and Component Schema definitions

- **Where:** `crates/core/src/openapi/spec.rs:83–147, 230–235`
- **What it says:**
  `OpenApiOperation` only declares `operation_id`, `summary`, `parameters: Vec<OpenApiParameter>`, and `responses: OpenApiResponses`.
  `OpenApiSpec` contains only `openapi`, `info`, and `paths`.
- **Why it's a problem:**
  While the generator correctly reflects path parameter matchers (resolving v02 DX M1), the OpenAPI specification lacks standard OpenAPI 3.1 capabilities:
  1. `OpenApiOperation` has no `requestBody` field. `POST`, `PUT`, and `PATCH` endpoints cannot describe JSON or form request schemas.
  2. `OpenApiSpec` has no `components` object (`components/schemas`, `components/securitySchemes`), meaning security schemes configured in `rume::auth` (Bearer, Cookie, Session) are omitted.
  3. Response definitions (lines 192–211) hardcode string descriptions without schema references to Problem Details (`$ref: "#/components/schemas/ProblemDetails"`).
- **Suggested fix:**
  Extend `OpenApiOperation` with `request_body: Option<OpenApiRequestBody>`, extend `OpenApiSpec` with `components: Option<OpenApiComponents>`, and register standard Problem Details schemas under `components/schemas`.

---

## Positive notes

- **RFC 9457 Problem Details discipline:** `crates/core/src/error.rs` is exceptionally rigorous. Error responses consistently use `application/problem+json`, emit the standard fields (`type`, `title`, `status`, `detail`, `instance`, `code`, `request_id`), log internal diagnostic error chains strictly server-side (S-M4), and sanitize client-visible error details against credential and connection-string leaks.
- **ParamValidationLayer fail-closed enforcement:** Parameter matching on `RouteSpec` (`crates/core/src/routing/spec.rs`) executes in a dedicated Tower service before the handler runs, correctly rejecting invalid input or malformed percent-encoding with 400 Problem Details without invoking downstream business logic.
- **Validation extractor taxonomy:** `crates/core/src/validation.rs` cleanly separates syntactical deserialization errors (400 Bad Request) from semantic `garde` validation failures (422 Unprocessable Entity with structured field-level error details) across JSON, Query, Path, and Form extractors.
- **Strictly bounded metrics cardinality:** `crates/core/src/metrics/registry.rs` avoids high-cardinality label explosion by partitioning metrics strictly by HTTP method and status code, avoiding raw path parameter keys; latency histograms use fixed static buckets, and `InFlightGuard` guarantees RAII decrement on dropped futures.
- **Health check data leakage prevention:** `crates/core/src/routing/health.rs` explicitly strips sensitive diagnostic metadata, database connection strings, and debug info from public `/health/ready` responses, exposing only safe overall status strings to HTTP clients.

---

## Verdict

**Not ready to proceed / ship as-is.**

While the RFC 9457 error contracts, validation extractors, and metrics pipeline are production-grade, **one Critical** and **three Major** defects must be addressed in a remediation pass:

1. **C1** — Enforce positive readiness gating in `/health/ready` (`availability.state() == AvailabilityState::Ready`) so that booting pods do not report healthy and receive traffic before startup hooks finish.
2. **M1** — Lazily evaluate OpenAPI spec generation in `Router::openapi()` so that routes registered after `.openapi()` and merged subrouters are included in `/openapi.json`.
3. **M2** — Align the `Matcher::Slug` OpenAPI regex pattern with `Matcher::Slug::validate` runtime validation to eliminate contract contradictions on underscores, casing, and slug length.
4. **M3** — Introduce in-flight coalescing and eliminate redundant read-lock loops in `HealthChecks::run()` to prevent cache stampedes under concurrent health probes.
