# Task 07 — App & Facade Wiring, End-to-End Integration Tests & Docs Verification

## Goal

Wire all four Milestone A deliverables into `rume-app` (`RumeBuilder`) and `rume` (`crates/core` facade + prelude), apply the approved **2 MiB default transport-level body limit** (with `axum::extract::DefaultBodyLimit::disable()` so `BodyLimitLayer` is the single source of truth), enforce the exact 4-layer framework middleware onion order, add end-to-end integration tests exercising all Milestone A pillars together, and verify `README.md` accuracy.

## Files touched

- `crates/app/src/app/builder/mod.rs`
- `crates/app/src/app/builder/router.rs`
- `crates/app/src/app/observability.rs`
- `crates/core/src/lib.rs`
- `crates/core/src/prelude.rs`
- `crates/core/tests/integration.rs`
- `crates/core/tests/integration/http_baseline.rs` (new)
- `README.md` (verify and update only if needed so claims match shipped APIs)

## Deliverable

1. **`RumeBuilder` ergonomic wiring (`crates/app/src/app/builder/mod.rs` & `router.rs`)**:
   - **Body limit (Approved decision #1)**:
     - Add `body_limit: Option<usize>` field on `RumeBuilder` defaulting to `Some(rume_http::transport::DEFAULT_BODY_LIMIT_BYTES)` (2 MiB).
     - Add `.body_limit(mut self, max_bytes: usize) -> Self` and `.without_body_limit(mut self) -> Self`.
     - **Axum `DefaultBodyLimit` interaction**: Always apply `axum::extract::DefaultBodyLimit::disable()` in `build_router_with_resolved` so `BodyLimitLayer` is the single source of truth when `body_limit` is `Some(max_bytes)` (allowing `.body_limit(> 2 MiB)` to actually work on `Json`/`Bytes`/`String` extractors) and `.without_body_limit()` genuinely disables the cap. Record a `Why:` comment explaining this interaction.
   - **CORS & Compression**:
     - Add `cors: Option<rume_http::transport::CorsConfig>` and `compression: Option<rume_http::transport::CompressionConfig>` fields on `RumeBuilder` (opt-in, `None` by default).
     - Add `.cors(mut self, config: CorsConfig) -> Self` and `.compression(mut self, config: CompressionConfig) -> Self`.
     - Validate `CorsConfig::build_layer()` fail-closed inside **both** `build_router_with_resolved` and `run_inspect_mode` (`crates/app/src/app/builder/mod.rs`, so `rume --inspect` also fails closed on invalid CORS configs), returning `BuildError::Cors(CorsConfigError)`.
   - **Exact 4-layer framework onion order (in Axum `Router::layer` call order, inner -> outer, so request entry executes 4 -> 3 -> 2 -> 1 -> user middleware -> handlers)**:
     1. `DefaultBodyLimit::disable()` + `BodyLimitLayer` (when `body_limit` is `Some(max_bytes)`) — rejects oversized bodies before user middleware runs.
     2. `CorsLayer` (when `cors` is `Some`) — short-circuits `OPTIONS` preflights before `BodyLimitLayer` and user middleware; attaches CORS headers to `413` and handler error responses.
     3. `ObservabilityLayer` (with `redaction` when configured) — traces/logs all requests (including preflights and `413`s), attaches `X-Request-ID`, and inspects uncompressed `application/problem+json` bodies to inject `request_id`.
     4. `CompressionLayer` (when `compression` is `Some`, outermost, applied last) — compresses responses after `request_id` injection and appends `Vary: accept-encoding` without clobbering `Vary: origin`.
     - Record a `Why:` comment in `crates/app/src/app/builder/router.rs` documenting this 4-layer order.
   - **Redaction**:
     - Add `redaction: Option<rume_observability::RedactionConfig>` on `RumeBuilder` with `.redaction(mut self, config: RedactionConfig) -> Self`.
     - Pass `redaction` into `ObservabilityLayer::with_redaction(...)` (or `ObservabilityLayer::new()` when `None`).
   - **Metrics (`/metrics`) (Approved decision #2)**:
     - Add `.metrics(self, path: &str, registry: rume_observability::MetricsRegistry) -> Self` mounting the Prometheus/OpenMetrics scrape endpoint open by default.
   - **OpenAPI Docs UI (Approved decision #3)**:
     - Add `.openapi_docs(self, docs_path: &str, spec_path: &str, info: OpenApiInfo, config: OpenApiDocsConfig) -> Self` (mounting both the `/openapi.json` spec via `.openapi(spec_path, info)` and the Docs UI via `openapi_docs_routes(docs_path, config)`), plus `.openapi_ui(self, docs_path: &str, config: OpenApiDocsConfig) -> Self` when the spec is mounted separately.
2. **Facade re-exports (`crates/app/src/app/observability.rs`, `crates/core/src/lib.rs`, `crates/core/src/prelude.rs`)**:
   - Re-export `rume_http::transport` as `rume::transport`.
   - Re-export `rume_observability::prometheus` and `rume_observability::redaction` (both at `rume::` and `rume::app::observability` where appropriate).
   - Re-export `rume_macros::HasOpenApiSchema` at `rume::HasOpenApiSchema` (alongside `embed_migrations`, `sql_assets`) and in `rume::prelude`.
   - Convert `crates/core/src/lib.rs:32` from `pub use rume_http::openapi;` to `pub mod openapi { pub use rume_http::openapi::*; pub use rume_macros::HasOpenApiSchema; }` (matching `pub mod routing` at line 55) so `rume::openapi::HasOpenApiSchema` exposes both the trait and derive without violating the L4->L6 DAG.
   - Ensure `rume::routing::{metrics_routes, metrics_routes_at}` and `rume::openapi::{OpenApiDocsConfig, OpenApiUi, openapi_docs_routes}` are accessible through `rume`.
3. **End-to-end integration test suite (`crates/core/tests/integration/http_baseline.rs`)**:
   - Registered in `crates/core/tests/integration.rs`.
   - Exercises all 4 Milestone A pillars end-to-end through `Rume::new()` / `TestApp` (including `#[derive(rume::HasOpenApiSchema)]` with `#[serde(rename_all = "camelCase")]` and `#[garde(...)]` rules attached via `RouteSpec::with_body::<T>()`).
4. **Documentation check (`README.md`)**:
   - Verify `README.md:18` ("Prometheus/OTLP") is now backed by real Prometheus/OpenMetrics exposition and check if any quick-start/subsystem bullet needs a one-line accuracy update.

## Constraints honoured

- `#![forbid(unsafe_code)]` in `crates/app/src/lib.rs` and `crates/core/src/lib.rs`.
- Preserve all existing `crates/app` tests (`crates/app/src/app/builder/tests.rs`) and all existing `crates/core` integration suites (`crates/core/tests/integration.rs`).
- Specifically verify no existing test sends a > 2 MiB body to `RumeBuilder` without `.body_limit(...)` / `.without_body_limit()`.
- `rules/40-verification.md`: run the **whole check** (fmt, clippy, unit, integration, doctests, feature-isolated backend matrix).
- `rules/70-code-rationale.md`: document `Why:` on the transport/observability middleware onion order in `crates/app/src/app/builder/router.rs`.

## Named tests (bodies omitted)

In `crates/core/tests/integration/http_baseline.rs`:
- `e2e_default_body_limit_rejects_over_2mib_and_respects_override_and_disable` — verifies `Rume::new()` rejects a `2 MiB + 1` payload with `413 Payload Too Large`, `.body_limit(1024)` rejects `1025` bytes with `413` while accepting `1024`, `.body_limit(3 * 1024 * 1024)` accepts `2 MiB + 1` on a `Bytes`/`String` extractor handler (proving `DefaultBodyLimit::disable()` is active), and `.without_body_limit()` accepts payloads over 2 MiB.
- `e2e_cors_and_compression_preserve_problem_details_request_id` — enables `.cors(...)` and `.compression(CompressionConfig::default().with_min_size_bytes(0))` on `Rume::new()`, sends a request triggering an RFC 9457 `AppError` with `Accept-Encoding: gzip`, decompresses the response, and asserts `request_id` was injected into the JSON body AND `Access-Control-Allow-Origin` + `Content-Encoding: gzip` headers are present.
- `e2e_invalid_cors_config_fails_closed_at_build_router` — configures `CorsConfig` with wildcard origin + credentials, asserts `build_router` returns `Err(BuildError::...)`.
- `e2e_metrics_endpoint_exposes_prometheus_and_openmetrics_from_live_requests` — mounts `MetricsLayer` + `.metrics("/metrics", registry)` on `Rume::new()`, sends requests to an app route, scrapes `GET /metrics` (both default Prometheus and `Accept: application/openmetrics-text`), asserts `http_requests_total` and `http_request_duration_seconds_bucket` are present and `/metrics` itself was bypassed by `MetricsLayer`.
- `e2e_derive_has_openapi_schema_serves_in_openapi_json_and_docs_ui` — defines a `#[derive(rume::HasOpenApiSchema, serde::Deserialize, garde::Validate)]` struct with `#[serde(rename_all = "camelCase")]`, `Option<String>`, `#[garde(email)]`, `#[garde(length(min = 3, max = 50))]`, `#[garde(range(min = 18, max = 120))]`, mounts via `RouteSpec::post("/users").with_body::<ValidatedJson<...>>()` + `.openapi_docs("/docs", "/openapi.json", ...)`, asserts `GET /openapi.json` contains exact camelCase properties, `required` list omitting optional fields, and garde constraints, and `GET /docs` returns `200 OK` HTML with Scalar UI pointing to `/openapi.json`.
- `e2e_redaction_config_masks_headers_and_json_body_without_breaking_streaming` — wires `.redaction(...)` on `Rume::new()`, verifies sensitive headers are redacted in logs and `RedactionConfig::redact_json_bytes` masks PII keys while a streaming endpoint returns intact chunks.

## Verification

```bash
cargo fmt --all --check
cargo clippy -j 2 --workspace --all-targets --all-features -- -D warnings
cargo test -j 2 --workspace --lib --all-features
cargo test -j 2 -p rume-macros --all-targets
cargo test -j 2 -p rume --test integration --all-features
cargo test -j 2 --workspace --doc --all-features
```

Expected: zero warnings across the entire workspace; all unit, macro, integration, and doc tests pass.

## Commit

*Only when the user asks:*

```text
feat(app,core): wire Milestone A transport, metrics, redaction, and OpenAPI docs into RumeBuilder and facade
```

## Done-when

- [ ] `RumeBuilder` applies a 2 MiB default transport-level body limit with `.body_limit(bytes)` and `.without_body_limit()`.
- [ ] `RumeBuilder` exposes `.cors(...)`, `.compression(...)`, `.redaction(...)`, `.metrics(...)`, and `.openapi_docs(...)` with fail-closed validation and documented onion layering order.
- [ ] `rume` facade and `prelude` re-export `transport`, `prometheus`, `redaction`, `HasOpenApiSchema` derive, `metrics_routes`, and `openapi_docs_routes`.
- [ ] All 6 end-to-end integration tests in `crates/core/tests/integration/http_baseline.rs` pass.
- [ ] Full workspace verification commands pass with zero warnings.
