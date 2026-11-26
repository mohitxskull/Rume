# Plan 005 Review — Milestone A: HTTP & Observability Baseline

**Subject:** [`docs/plans/005-http-observability-baseline/`](file:///home/skull/Projects/rume/docs/plans/005-http-observability-baseline/README.md)  
**Verdict:** **Changes Requested**  
**Findings:** 1 Critical (`C1`), 4 Major (`M1`–`M4`), 4 Minor (`m1`–`m4`)

---

## Executive Summary

The plan is well-structured, honors `plan-authoring/SKILL.md` cleanly (all required sections, concrete codebase citations, no boilerplate or cross-task type signatures, task index matches disk, checkable done-when boxes), and achieves genuine pairwise file disjointness across Wave 1 (`02`–`06`). The core decision to place `CompressionLayer` outside `ObservabilityLayer` is verified correct against Axum's outer-last wrapping semantics (`crates/app/src/app/builder/router.rs:378-384`) and `ObservabilityLayer`'s `application/problem+json` inspection (`crates/observability/src/layer.rs:214-313`).

However, five concrete issues should be fixed in the plan before Wave 0 starts: one Critical Axum `DefaultBodyLimit` interaction that will break `.body_limit(>2MiB)` and `.without_body_limit()`, and four Major gaps covering Wave 0 test dependencies, full 4-layer middleware onion ordering, Wave 0/Wave 1 symbol alignment, and two CORS fail-closed edge cases.

---

## Findings

### Critical

#### `C1` — Axum's built-in `DefaultBodyLimit` (2 MiB) breaks `.body_limit(>2MiB)` and `.without_body_limit()` (Task 07)
- **Where:** `07-app-facade-wiring-and-e2e.md` (Deliverable §1 Body limit; Named test `e2e_default_body_limit_rejects_over_2mib_and_respects_override_and_disable`)
- **Problem:** In Axum 0.8 (`axum = "=0.8.9"`), extractors (`Json`, `String`, `Bytes`, `Form`) enforce Axum's own `axum::extract::DefaultBodyLimit` (2 MiB) independently of `tower_http::limit::RequestBodyLimitLayer`. Task 07 only adds `BodyLimitConfig::new(max_bytes).build_layer()` when `body_limit` is `Some(max_bytes)` and omits it when `None`. As written:
  - `Rume::new().body_limit(10 * 1024 * 1024)` will still return `413 Payload Too Large` at `2 MiB + 1` on any handler using `Json`/`Bytes`/`String`.
  - `Rume::new().without_body_limit()` will still return `413 Payload Too Large` at `2 MiB + 1`, failing Task 07's own E2E test.
- **Required fix:** In `build_router_with_resolved` (`crates/app/src/app/builder/router.rs`), always apply `axum::extract::DefaultBodyLimit::disable()` when `RumeBuilder` manages body limits so `BodyLimitLayer` is the single source of truth when `Some(max_bytes)` and `.without_body_limit()` genuinely disables the cap. Record a `Why:` comment explaining the Axum `DefaultBodyLimit` interaction.

---

### Major

#### `M1` — Wave 0 omits decompression `[dev-dependencies]`, breaking Wave 1 worktree isolation for Task 02 (and needed by Task 07)
- **Where:** `01-shared-base-deps-and-skeleton.md` (Deliverable §1), `02-http-transport-middleware.md` (Named test `compression_encodes_gzip_brotli_and_zstd_when_accepted`), `07-app-facade-wiring-and-e2e.md` (Named test `e2e_cors_and_compression_preserve_problem_details_request_id`)
- **Problem:** Task 02's unit test asserts *"that the decompressed payload equals the original body"* for `gzip`, `br`, and `zstd`, and Task 07's E2E test decompresses a `gzip` response. However, Task 01 only enables `tower-http` `compression-*` features in `crates/http/Cargo.toml` and adds no decompression dev-dependency to `crates/http/Cargo.toml` or `crates/core/Cargo.toml`. Because Task 02 is forbidden from editing `Cargo.toml`, it cannot implement its named test without violating Wave 1 isolation.
- **Required fix:** In Task 01 (Wave 0), add decompression support to `[dev-dependencies]` of `crates/http/Cargo.toml` and `crates/core/Cargo.toml` — e.g., `tower-http = { workspace = true, features = ["decompression-gzip", "decompression-br", "decompression-zstd"] }` (which reuses the `async-compression` codecs already pulled by `compression-*` and adds zero new license risk under `deny.toml`).

#### `M2` — Wave 2 middleware onion order is underspecified for `BodyLimitLayer` and `CorsLayer` (Task 07)
- **Where:** `07-app-facade-wiring-and-e2e.md` (Deliverable §1 Layering order)
- **Problem:** Task 07 specifies `CompressionLayer` outside `ObservabilityLayer` and `CorsLayer` outside user middleware, but does not specify where `BodyLimitLayer` sits or where `CorsLayer` sits relative to `ObservabilityLayer` and `BodyLimitLayer`.
  - If `BodyLimitLayer` wraps outside `CorsLayer`, browsers receive `413 Payload Too Large` without `Access-Control-Allow-Origin` and surface an opaque CORS failure instead of `413`.
  - If `CorsLayer` wraps outside `ObservabilityLayer`, preflight `OPTIONS` requests short-circuit before tracing spans and `X-Request-ID` are attached.
- **Required fix:** Specify the exact 4-layer framework onion in Task 07 (in Axum `Router::layer` call order, inner -> outer, so request entry executes 4 -> 3 -> 2 -> 1 -> user middleware -> handlers):
  1. `BodyLimitLayer` (+ `DefaultBodyLimit::disable()`) — rejects oversized bodies before user middleware runs.
  2. `CorsLayer` — short-circuits `OPTIONS` preflight before `BodyLimitLayer` and user middleware; attaches CORS headers to `413` and handler error responses.
  3. `ObservabilityLayer` — traces/logs all requests (including preflights and `413`s), attaches `X-Request-ID`, and inspects uncompressed `application/problem+json` bodies to inject `request_id`.
  4. `CompressionLayer` (outermost, applied last) — compresses responses after `request_id` injection and appends `Vary: accept-encoding` without clobbering `Vary: origin`.

#### `M3` — Wave 0 vs Wave 1 public export mismatches could force Wave 1 edits to `lib.rs` / `mod.rs`
- **Where:** `01-shared-base-deps-and-skeleton.md` (Deliverable §2) vs `03-prometheus-metrics-exposition.md` and `04-http-pii-redaction.md`
- **Problem:** Wave 1 tasks `03` and `04` are forbidden from editing `lib.rs` and `mod.rs`, yet their deliverables diverge slightly from Task 01's listed exports:
  1. Task 01 (`rume-observability::prometheus`) lists `render_metrics`, whereas Task 03 introduces `render_registry(&MetricsRegistry, PrometheusFormat) -> String`.
  2. Task 01 (`rume-http::routing::metrics`) lists `metrics_routes` and `metrics_handler`, omitting `metrics_routes_at` which Task 03 adds.
  3. Task 01 (`rume-observability::redaction`) lists `redact_headers`, `redact_json_value`, `redact_json_bytes` as module-level exports, whereas Task 04 defines them as methods on `RedactionConfig`.
- **Required fix:** Reconcile the exact exported symbol names in Task 01 with Tasks 03 and 04 (stub and re-export `render_registry`, `render_metrics`, and `metrics_routes_at` in Wave 0; decide whether `redact_*` are methods on `RedactionConfig` only or also top-level default-config helpers).

#### `M4` — CORS fail-closed validation omits `expose_headers(Any)` + `allow_credentials(true)` and literal `"*"` in string lists (Task 02)
- **Where:** `02-http-transport-middleware.md` (Deliverable §1 CORS)
- **Problem:**
  1. In `tower-http 0.6`, combining `expose_headers(Any)` with `allow_credentials(true)` panics at runtime per the Fetch spec, just like wildcard origin/method/header. Task 02 currently lists only origin, method, and header wildcard checks.
  2. If a caller passes literal `"*"` in `allow_origins(["*"])`, `HeaderValue::from_str("*")` succeeds as a valid header value, bypassing the `AllowAnyOrigin` + `allow_credentials(true)` check.
- **Required fix:** Reject `expose_headers(Any)` + `allow_credentials(true)` with `CorsConfigError`, and reject literal `"*"` in `allow_origins` / `allow_methods` / `allow_headers` / `expose_headers` string lists (requiring the explicit wildcard opt-in).

---

### Minor

#### `m1` — `e2e_cors_and_compression_preserve_problem_details_request_id` will not compress a small `AppError` under `CompressionConfig::default()`
- **Where:** `07-app-facade-wiring-and-e2e.md` (Named test line 62)
- **Detail:** `CompressionConfig::default()` sets `min_size_bytes = 1024` (Task 02 line 27), whereas a typical RFC 9457 `AppError` JSON payload is ~120–200 bytes. The test must either trigger an `AppError` with a detail payload > 1 KiB or configure `min_size_bytes(0)` so `Content-Encoding: gzip` is actually emitted.

#### `m2` — OpenMetrics 1.0 counter `_total` family-name convention (Task 03)
- **Where:** `03-prometheus-metrics-exposition.md` (Deliverable §1)
- **Detail:** `MetricsRegistry` counter names already end in `_total` (`http_requests_total`, `health_checks_total`, `health_check_failures_total` in `crates/observability/src/metrics.rs:245-298`). In Prometheus 0.0.4, `# TYPE http_requests_total counter` is standard, but strict OpenMetrics 1.0 expects `# TYPE http_requests counter` / `# HELP http_requests ...` with sample lines carrying `http_requests_total`. Handle the `_total` suffix per format so OpenMetrics 1.0 output is spec-compliant.

#### `m3` — `#[derive(HasOpenApiSchema)]` handling of non-integer `#[garde(range(...))]` bounds (Task 05)
- **Where:** `05-derive-has-openapi-schema.md` (Deliverable §2)
- **Detail:** `OpenApiSchema::with_minimum` and `with_maximum` take `i64` (`crates/http/src/openapi/spec.rs:186-194`). Clarify that non-integer (e.g. `f64`) or non-literal `#[garde(range(...))]` bounds are ignored gracefully rather than generating uncompilable `with_minimum(0.5)` calls.

#### `m4` — Three small implementation details in Tasks 04 and 07
- **Task 04 (`crates/observability/src/layer.rs`):** Emit redacted request-header `tracing::debug!` events *inside* the `async move { ... }.instrument(span)` block (right before `inner.call(req).await?`), not outside `Box::pin`, so they attach to the `http_request` span. Document on `redact_json_bytes` that `None` means "do not log raw bytes" (fail closed).
- **Task 07 (`crates/app/src/app/builder/mod.rs`):** Validate `self.cors` inside `run_inspect_mode` as well as `build_router_with_resolved` so `rume --inspect` fails closed on invalid CORS configs.
- **Task 07 (`crates/core/src/lib.rs`):** `crates/core/src/lib.rs:32` currently uses `pub use rume_http::openapi;`. To re-export `rume_macros::HasOpenApiSchema` under `rume::openapi::HasOpenApiSchema` without violating the L4->L6 DAG, convert line 32 to a `pub mod openapi { pub use rume_http::openapi::*; pub use rume_macros::HasOpenApiSchema; }` block (matching `pub mod routing` at line 55).

---

## Verification Checklist (`plan-authoring/SKILL.md`)

- [x] Every task file has `Goal`, `Files touched`, `Verification`, `Commit`, `Done-when`.
- [x] No task file contains boilerplate or cross-task type signatures.
- [x] Constraints are cited by file/symbol/test rather than restated as prose.
- [x] `README.md` task index matches files on disk (`01`–`07`).
- [x] Done-when boxes are checkable by command or inspection.
- [x] `deny.toml` license allow-list covers `tower-http`, `async-compression`, `flate2`, `brotli`, and `zstd`; `#![forbid(unsafe_code)]` preserved across all crates.
- [ ] **Blocked on `C1`, `M1`–`M4`** before starting Wave 0.
