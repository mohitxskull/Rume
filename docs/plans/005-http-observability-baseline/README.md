# Plan 005 — Milestone A: HTTP & Observability Baseline

## Goal

Close the four Milestone A P0 gaps cleanly and idiomatically, executed **strictly sequentially on `main`** (one task at a time):

1. **HTTP transport middleware (`rume-http` + `rume-app` / `rume`)** — typed CORS (`CorsConfig`/`CorsLayer`), response compression (`CompressionConfig`/`CompressionLayer` supporting gzip, brotli, zstd), and transport-level request body-size limits (`BodyLimitConfig`/`BodyLimitLayer`) built on `tower-http = "=0.6.11"`, with a **2 MiB default transport-level body limit** wired into `RumeBuilder` (overridable via `.body_limit(bytes)` and `.without_body_limit()`).
2. **Prometheus / OpenMetrics `/metrics` exposition (`rume-observability` + `rume-http`)** — close the `README.md:18` gap by rendering Rume's `MetricsRegistry` (`opentelemetry_sdk` `ManualReader` / `MetricsSnapshot`) in Prometheus text exposition format (`text/plain; version=0.0.4; charset=utf-8`) and OpenMetrics 1.0 (`application/openmetrics-text; version=1.0.0; charset=utf-8`), exposed via an ergonomic, open-by-default `/metrics` route helper in `rume-http` and `RumeBuilder::metrics`.
3. **HTTP header & body PII redaction (`rume-observability` + `rume-app`)** — structured, configurable redaction (`RedactionConfig`) for HTTP tracing/logging (`Authorization`, `Proxy-Authorization`, `Cookie`, `Set-Cookie`, `X-API-Key`, `X-CSRF-Token`, plus custom headers) and opt-in bounded JSON body key redaction, integrated into `ObservabilityLayer` without breaking streaming or adding unconditional body buffering.
4. **`#[derive(HasOpenApiSchema)]` & OpenAPI Docs UI (`rume-macros` + `rume-http` + `rume-app`)** — procedural derive macro in `rume-macros` generating `HasOpenApiSchema` (handling `Option<T>`, `#[serde(...)]`, and `#[garde(...)]` constraints), plus a built-in OpenAPI Docs UI route helper supporting selectable `OpenApiUi::Scalar` (default) and `OpenApiUi::SwaggerUi` with pinned CDN URLs and configurable `script_url` overrides.

### Out of scope

- Milestone B (Auth: Argon2id, JWT/JWKS/OIDC, persistent `ApiKeyStore`; Tenancy: `PathPrefix`/`CachedDbTenantResolver`, Postgres RLS)
- Milestone C (Cron scheduler, `RumeClient`, `SqliteJobQueue`/`MysqlJobQueue`, `SqlSagaStore`)
- Milestone D (WebSockets/SSE `RealtimeHub`, Multipart/ObjectStorage, `#[derive(FromRow)]`, CLI `rume openapi`/`rume task`)

## Current state (read before executing)

Facts verified against the tree at plan time:

- **Workspace pins (`Cargo.toml` / `Cargo.lock`)**:
  - Edition `2024`, MSRV `1.96`, `#![forbid(unsafe_code)]` on all library crates.
  - `axum = "=0.8.9"`, `tower = "=0.5.3"`, `opentelemetry = "=0.33.1"`, `opentelemetry_sdk = "=0.33.1"`, `garde = "=0.23.0"`, `syn = "=3.0.10"`, `proc-macro2 = "=1.0.107"`, `quote = "=1.0.47"`.
  - `tower-http = "0.6.11"` is already resolved in `Cargo.lock` (not yet declared in root `[workspace.dependencies]`).
  - `opentelemetry-prometheus` is officially deprecated at `0.29` (unmaintained `protobuf` dependency; incompatible with `opentelemetry_sdk = "=0.33.1"`). Rendering Prometheus/OpenMetrics text directly from `ResourceMetrics` / `MetricsSnapshot` adds zero new dependencies and works without `feature = "otel"`.
- **Observability (`crates/observability/src/`)**:
  - `metrics.rs` already defines `MetricsRegistry` (backed by `SdkMeterProvider` + `ManualReader`), `MetricsRegistry::export_metrics() -> Vec<ResourceMetrics>`, `MetricsSnapshot`, and `MetricsLayer` (whose default `bypass_path` is already `"/metrics"`).
  - `layer.rs` defines `ObservabilityLayer` / `ObservabilityService`, which currently records `request_id`, `traceparent`, `tracestate`, `method`, `route`, `status`, `latency_ms`, and enforces a strict streaming body-integrity invariant on `application/problem+json` inspection (lines 221–313).
- **HTTP & OpenAPI (`crates/http/src/`)**:
  - `openapi/spec.rs` already defines `OpenApiSchema`, `HasOpenApiSchema` (with primitive/`Vec`/`Option`/`Uuid` impls), `OpenApiInfo`, `OpenApiSpec`, and `generate_openapi`.
  - `routing/spec.rs:130` already has `RouteSpec::with_body<T: HasOpenApiSchema>()`.
  - `routing/health.rs` establishes the house pattern for built-in route helpers (`health_routes<S>()` using `FromRef<S>`).
  - `validation.rs:549` shows a manual `HasOpenApiSchema` impl for `CreateUser` mirroring `#[garde(...)]` rules.
- **Macros (`crates/macro/src/lib.rs`)**:
  - Currently exports `sql_assets!` and `embed_migrations!`, referencing `::rume::...` paths; has `trybuild = "=1.0.125"` in `[dev-dependencies]`.
- **App & Facade (`crates/app/src/app/builder/router.rs`, `crates/core/src/lib.rs`)**:
  - `RumeBuilder::openapi(path, info)` already exists at `crates/app/src/app/builder/router.rs:284`.
  - `RumeBuilder::build_router_with_resolved` applies `ObservabilityLayer::new()` as the outermost framework layer (`crates/app/src/app/builder/router.rs:445`).
  - `crates/core/src/lib.rs` re-exports `openapi`, `metrics`, `routing`, `validation`, `resilience`, and `rume_macros::{embed_migrations, sql_assets}`.

## Global constraints

- **Workspace invariants**: `#![forbid(unsafe_code)]` in all library crates; Rust Edition 2024; MSRV `1.96`; concrete version pins (`=x.y.z`) in root `Cargo.toml`; all new dependencies must pass `deny.toml` license allow-list.
- **Strict 7-layer DAG**: L0 `rume-foundation` -> L1 `rume-observability` -> L2 `rume-data` -> L3 `rume-messaging`/`rume-security`/`rume-enterprise` -> L4 `rume-http` -> L5 `rume-app` -> L6 `rume` (`crates/core`) + `rume-macros` (`crates/macro`). Never introduce upward or cyclic dependencies.
- **Build concurrency**: Every `cargo` command MUST pass `-j 2`.
- **Zero-warning bar** (`rules/80-quality-bar.md`): `cargo fmt --all --check` and `cargo clippy -j 2 --workspace --all-targets --all-features -- -D warnings` must pass with zero warnings.
- **Execution mode (strictly sequential on `main`)**:
  - Execute tasks `01` through `07` strictly in order on `main` (one implementer subagent at a time, no parallel worktrees, per `rules/30-git.md` and `rules/50-agent-conduct.md`).
  - Task `01` pins dependencies and scaffolds the compiling skeleton; tasks `02`–`06` implement each subsystem cleanly against that skeleton; task `07` wires everything into `rume-app` / `rume` and runs full end-to-end verification.
- **Commit discipline** (`rules/30-git.md`): Conventional Commits; no commits unless explicitly asked or announced up front by the controller.
- **TDD & fails-before** (`rules/50-agent-conduct.md`, `rules/80-quality-bar.md`): Write tests first (or against Task `01` stubs so they fail before implementation), then implement minimal clean code.

## Constraints honoured

- **Approved product decisions (Human)**:
  1. `RumeBuilder::build_router` applies a **2 MiB** (`2 * 1024 * 1024`) default transport-level `BodyLimitLayer`, overridable via `.body_limit(bytes)` and `.without_body_limit()`.
  2. `/metrics` is **open by default** (matching `crates/http/src/routing/health.rs` and `RumeBuilder::openapi`), composable with any auth/IP middleware or internal listener.
  3. OpenAPI Docs UI exposes a selectable `OpenApiUi` enum (**Scalar** default + **SwaggerUi**), serving minimal HTML with a pinned CDN URL and configurable `script_url` override.
- **Observability streaming invariant**: `crates/observability/src/layer.rs:221-313` (`observability_service_preserves_body_on_oversized_stream`, `observability_service_preserves_stream_error_on_inspected_problem_details`) must remain untouched and passing.
- **Observability W3C & baggage isolation**: `crates/observability/src/layer.rs` tests (`observability_layer_extracts_w3c_traceparent_into_otel_span_and_response`, `observability_layer_emits_child_span_id_with_and_without_otel_subscriber`, `observability_fallback_preserves_future_trace_version_and_baggage`) must remain passing.
- **Metrics cardinality & bypass**: `crates/observability/src/metrics.rs:38` (`http_method_label`) and `MetricsLayer::new` default bypass path `"/metrics"` (`crates/observability/src/metrics.rs:591`) must be preserved.
- **OpenAPI 3.1 compatibility**: `crates/http/src/openapi/spec.rs` (`openapi_uses_camelcase_operation_fields_and_segment_colon_conversion`) and `crates/http/src/validation.rs:560` (`test_openapi_validation_synergy_reflects_garde_rules`) must remain passing.
- **Existing `RumeBuilder` middleware & route semantics**: `crates/app/src/app/builder/middleware.rs` (fail-closed validation) and `crates/app/src/app/builder/router.rs` (duplicate route detection, `MatchedPath` span recording, `with_state` snapshot) must remain passing.
- **Rules**: `rules/10-tech-stack.md`, `rules/40-verification.md`, `rules/50-agent-conduct.md`, `rules/70-code-rationale.md`, `rules/80-quality-bar.md`, `rules/85-minimalism.md`, `rules/testing.md`.

## Task index

- `01-shared-base-deps-and-skeleton.md` — Pin `tower-http = "=0.6.11"` in workspace `Cargo.toml` (including decompression dev-deps), wire crate manifests, and scaffold compiling module/export skeletons.
- `02-http-transport-middleware.md` — Implement typed CORS, response compression (gzip/brotli/zstd), and request body-size limit middleware in `rume-http::transport`.
- `03-prometheus-metrics-exposition.md` — Implement Prometheus/OpenMetrics text renderer in `rume-observability::prometheus` and `/metrics` route helper in `rume-http::routing::metrics`.
- `04-http-pii-redaction.md` — Implement structured HTTP header & bounded JSON body PII redaction in `rume-observability::redaction` and wire into `ObservabilityLayer`.
- `05-derive-has-openapi-schema.md` — Implement `#[derive(HasOpenApiSchema)]` proc-macro in `rume-macros` supporting structs, `Option<T>`, `#[serde(...)]`, and `#[garde(...)]` rules.
- `06-openapi-docs-ui.md` — Implement OpenAPI Docs UI (`OpenApiUi::Scalar` default + `OpenApiUi::SwaggerUi`, pinned CDN + `script_url` override) in `rume-http::openapi::docs`.
- `07-app-facade-wiring-and-e2e.md` — Wire transport middleware (including default 2 MiB body limit + `DefaultBodyLimit::disable()` and 4-layer onion order), `/metrics`, redaction, and OpenAPI Docs UI into `rume-app` and `rume` facade, add end-to-end integration tests, and verify `README.md`.

## Done-when checklist

- [ ] `tower-http = "=0.6.11"` is pinned in root `Cargo.toml` and passes `deny.toml` license rules.
- [ ] `rume-http::transport` provides typed `CorsConfig`/`CorsLayer`, `CompressionConfig`/`CompressionLayer` (gzip/brotli/zstd), and `BodyLimitConfig`/`BodyLimitLayer`.
- [ ] `RumeBuilder` applies a default 2 MiB transport-level request body limit, overridable via `.body_limit(bytes)` and `.without_body_limit()`.
- [ ] `rume-observability::prometheus` renders counters, gauges, and explicit-bucket histograms from `MetricsRegistry` in valid Prometheus 0.0.4 and OpenMetrics 1.0 text formats, and `rume-http::routing::metrics_routes` / `RumeBuilder::metrics` exposes `/metrics` open by default.
- [ ] `rume-observability::redaction` redacts sensitive headers (`Authorization`, `Cookie`, `Set-Cookie`, etc.) and provides bounded JSON body key redaction, wired into `ObservabilityLayer` without breaking streaming.
- [ ] `#[derive(HasOpenApiSchema)]` in `rume-macros` compiles for structs with primitives, `Option<T>`, `Vec<T>`, nested types, `#[serde(rename/rename_all/skip)]`, and `#[garde(...)]` rules, with `trybuild` compile-fail tests for unsupported inputs.
- [ ] `rume-http::openapi::docs` serves Scalar (default) and Swagger UI HTML pages pointing to `/openapi.json` with pinned CDN URLs and configurable `script_url` overrides, wired via `RumeBuilder::openapi_docs`.
- [ ] All verification commands below pass cleanly with zero warnings.

## Verification commands

```bash
cargo fmt --all --check
cargo clippy -j 2 --workspace --all-targets --all-features -- -D warnings
cargo test -j 2 --workspace --lib --all-features
cargo test -j 2 -p rume-macros --all-targets
cargo test -j 2 -p rume --test integration --all-features
cargo test -j 2 --workspace --doc --all-features
```

## Flagged gaps

- **Middleware onion order & Axum `DefaultBodyLimit` interaction**: Task `07` enforces the exact 4-layer framework onion in `Router::layer` call order (inner -> outer: `BodyLimitLayer` + `axum::extract::DefaultBodyLimit::disable()` -> `CorsLayer` -> `ObservabilityLayer` -> `CompressionLayer`) so `BodyLimitLayer` is the single source of truth for `.body_limit(>2MiB)` / `.without_body_limit()`, `CorsLayer` attaches headers to `413`s and short-circuits preflights, `ObservabilityLayer` traces all requests and injects `request_id` into uncompressed `application/problem+json` bodies, and `CompressionLayer` compresses responses last.
- **CSP headers vs OpenAPI Docs UI**: `SecurityHeadersConfig::default()` sets `Content-Security-Policy: default-src 'self'` (`crates/security/src/security/headers.rs:54`). When an app enables both `SecurityHeadersLayer` and CDN-backed `OpenApiDocsConfig`, the docs route will need a relaxed CSP or a self-hosted `script_url`; document this explicitly in `openapi::docs` rustdoc.
