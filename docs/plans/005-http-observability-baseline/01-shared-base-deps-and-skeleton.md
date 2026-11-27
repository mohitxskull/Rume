# Task 01 — Shared Base: Workspace Dependencies & Compiling Skeleton

## Goal

Pin `tower-http = "=0.6.11"` in root `Cargo.toml`, update `Cargo.lock` and crate manifests (including decompression `[dev-dependencies]` for compression round-trip tests), and scaffold the compiling module declarations, public type/function signatures, and `todo!()` stubs across `rume-http`, `rume-observability`, and `rume-macros` so Tasks `02`–`07` compile against real types from day one.

## Files touched

- `Cargo.toml`
- `Cargo.lock`
- `crates/http/Cargo.toml`
- `crates/core/Cargo.toml`
- `crates/http/src/lib.rs`
- `crates/http/src/transport/mod.rs` (new)
- `crates/http/src/transport/cors.rs` (new stub)
- `crates/http/src/transport/compression.rs` (new stub)
- `crates/http/src/transport/body_limit.rs` (new stub)
- `crates/http/src/routing/mod.rs`
- `crates/http/src/routing/metrics.rs` (new stub)
- `crates/http/src/openapi/mod.rs`
- `crates/http/src/openapi/docs.rs` (new stub)
- `crates/observability/src/lib.rs`
- `crates/observability/src/prometheus.rs` (new stub)
- `crates/observability/src/redaction.rs` (new stub)
- `crates/macro/src/lib.rs`
- `crates/macro/src/openapi_schema.rs` (new stub)

## Deliverable

1. **Concrete dependency pin in root `Cargo.toml`**:
   - Add `tower-http = { version = "=0.6.11", default-features = false, features = ["cors", "compression-gzip", "compression-br", "compression-zstd", "limit"] }` under `[workspace.dependencies]`.
   - Add `tower-http.workspace = true` to `crates/http/Cargo.toml` `[dependencies]` (not `rume-app`; `rume-app` consumes Rume's typed wrappers from `rume-http::transport`).
   - Add `tower-http = { workspace = true, features = ["decompression-gzip", "decompression-br", "decompression-zstd"] }` to `[dev-dependencies]` of `crates/http/Cargo.toml` and `crates/core/Cargo.toml` so Tasks `02` and `07` can round-trip decompress `gzip`, `br`, and `zstd` responses without adding extra codec dependencies (reuses `async-compression` already pulled by `compression-*`).
   - Verify all newly pulled transitive crates (`async-compression`, `brotli`, `zstd`, etc.) comply with `deny.toml` licenses.
2. **Compiling module & signature skeleton**:
   - `crates/http/src/lib.rs`: declare `pub mod transport;`.
   - `crates/http/src/transport/{mod,cors,compression,body_limit}.rs`: public config/layer types (`CorsConfig`, `CorsConfigError`, `CorsLayer`, `CompressionConfig`, `CompressionLayer`, `BodyLimitConfig`, `BodyLimitLayer`, `DEFAULT_BODY_LIMIT_BYTES`) with `todo!()` method bodies.
   - `crates/http/src/routing/mod.rs` + `crates/http/src/routing/metrics.rs`: public `metrics_routes<S>()`, `metrics_routes_at<S>(path: &str)`, and `metrics_handler` (extracting `MetricsRegistry` via `FromRef<S>`) with `todo!()` bodies.
   - `crates/http/src/openapi/mod.rs` + `crates/http/src/openapi/docs.rs`: public `OpenApiUi` (`Scalar`, `SwaggerUi`), `OpenApiDocsConfig`, `openapi_docs_html`, and `openapi_docs_routes` with `todo!()` bodies.
   - `crates/observability/src/lib.rs` + `crates/observability/src/{prometheus,redaction}.rs`:
     - `prometheus`: `PrometheusFormat` (`Text004`, `OpenMetrics100`), `PROMETHEUS_CONTENT_TYPE`, `OPENMETRICS_CONTENT_TYPE`, `render_prometheus`, `render_openmetrics`, `render_metrics`, `render_registry`.
     - `redaction`: `RedactionConfig` (with `redact_headers`, `redact_json_value`, `redact_json_bytes` methods), `DEFAULT_REDACTED_PLACEHOLDER`, `DEFAULT_SENSITIVE_HEADERS`, `DEFAULT_SENSITIVE_JSON_KEYS`.
   - `crates/macro/src/lib.rs` + `crates/macro/src/openapi_schema.rs`: declare `mod openapi_schema;` and `#[proc_macro_derive(HasOpenApiSchema, attributes(openapi, serde, garde))] pub fn derive_has_openapi_schema(input: TokenStream) -> TokenStream` delegating to `openapi_schema::expand`.
3. Every stub compiles cleanly under `cargo check -j 2 --workspace --all-targets --all-features` and `cargo clippy -j 2 --workspace --all-targets --all-features -- -D warnings` (using `#[allow(unused_variables, dead_code)]` only on `todo!()` stubs with a comment noting removal in Tasks `02`–`06`).

## Constraints honoured

- Concrete pin rule (`rules/10-tech-stack.md`, root `Cargo.toml`): exact `=0.6.11` matching existing `Cargo.lock` entry.
- `#![forbid(unsafe_code)]` in `crates/http/src/lib.rs`, `crates/observability/src/lib.rs`, `crates/macro/src/lib.rs`.
- Strict 7-layer DAG: `rume-observability` (L1) does NOT depend on `rume-http` (L4); `rume-http` (L4) already depends on `rume-observability` (L1) (`crates/http/Cargo.toml:11`).
- `rules/85-minimalism.md`: plan skeleton is explicitly exempt from minimalism rules; keep stubs lean and accurate so Tasks `02`–`06` focus purely on their implementation modules.

## Code-only decisions

- `pub const DEFAULT_BODY_LIMIT_BYTES: usize = 2 * 1024 * 1024;` (2 MiB) — Why: matches Axum's `DefaultBodyLimit` default while enforcing the cap at the HTTP transport layer (`413 Payload Too Large`) before handlers stream unbounded request bodies.
- `pub const PROMETHEUS_CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";`
- `pub const OPENMETRICS_CONTENT_TYPE: &str = "application/openmetrics-text; version=1.0.0; charset=utf-8";`
- `pub const DEFAULT_REDACTED_PLACEHOLDER: &str = "[REDACTED]";`

## Verification

```bash
cargo fmt --all --check
cargo check -j 2 --workspace --all-targets --all-features
cargo clippy -j 2 --workspace --all-targets --all-features -- -D warnings
cargo test -j 2 --workspace --lib --all-features
```

Expected: zero warnings, all existing unit tests pass, `Cargo.lock` updated cleanly. If `cargo-deny` is installed in the environment, also run `cargo deny check licenses`.

## Commit

*Only when the user asks:*

```text
build(http,observability,macro): pin tower-http 0.6.11 and scaffold Plan 005 compiling skeleton
```

## Done-when

- [ ] `tower-http = "=0.6.11"` is pinned in root `Cargo.toml`, added to `crates/http/Cargo.toml`, and decompression dev-deps are added to `crates/http/Cargo.toml` and `crates/core/Cargo.toml`.
- [ ] All Tasks `02`–`06` module files exist as compiling stubs wired into their parent `lib.rs` / `mod.rs`.
- [ ] `cargo check -j 2 --workspace --all-targets --all-features` and `cargo clippy -j 2 --workspace --all-targets --all-features -- -D warnings` pass with zero warnings.
- [ ] Existing workspace unit tests remain green.
