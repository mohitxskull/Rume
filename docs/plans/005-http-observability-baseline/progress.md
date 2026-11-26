# Plan ledger — plan: docs/plans/005-http-observability-baseline/

- **Execution mode:** Strictly sequential on `main`, one implementer subagent at a time (per user override + `rules/30-git.md`).
- **Commits:** Authorized per task on `main` (local commits only, never push — announced up front under `/goal` run).
- **Base commit:** `b7c2349` (verified via `git rev-parse HEAD`).

## Pre-flight scan

| Pair / Task | Shared File or Interface | Scan Result |
|---|---|---|
| `01` → `02` | `crates/http/Cargo.toml`, `crates/http/src/transport/*` | Clean: `01` pins `tower-http = "=0.6.11"` (+ decompression dev-deps) and scaffolds `transport` stubs; `02` implements `cors`, `compression`, `body_limit`. |
| `01` → `03` | `crates/observability/src/prometheus.rs`, `crates/http/src/routing/metrics.rs` | Clean: `01` exports `render_prometheus`, `render_openmetrics`, `render_metrics`, `render_registry`, `metrics_routes`, `metrics_routes_at`, `metrics_handler` (reconciled per `M3`). |
| `01` → `04` | `crates/observability/src/redaction.rs`, `crates/observability/src/layer.rs` | Clean: `01` scaffolds `RedactionConfig` method stubs (`redact_headers`, `redact_json_value`, `redact_json_bytes`); `04` implements and wires into `ObservabilityLayer`. |
| `01` → `05` | `crates/macro/src/openapi_schema.rs`, `crates/macro/src/lib.rs` | Clean: `01` scaffolds `#[derive(HasOpenApiSchema)]` entry point; `05` implements proc-macro + `trybuild` tests. |
| `01` → `06` | `crates/http/src/openapi/docs.rs` | Clean: `01` scaffolds `OpenApiUi`/`OpenApiDocsConfig` stubs; `06` implements Scalar + SwaggerUi HTML & routes. |
| `02..06` → `07` | `crates/app`, `crates/core` (`rume` facade), `README.md` | Clean: `07` consumes all public APIs from `02..06`, applies exact 4-layer onion (`BodyLimitLayer` + `DefaultBodyLimit::disable()` → `CorsLayer` → `ObservabilityLayer` → `CompressionLayer`), re-exports `openapi::HasOpenApiSchema`, and runs full E2E suite. |
| Self-consistency (`01`..`07`) | All 7 task files | Clean: Verified by `review/plan-review.md` and all `C1`, `M1`–`M4`, `m1`–`m4` findings resolved before execution. |

## Execution Log

- Task 1: complete (b7c2349..c8881cd, review clean) — implementer `79737932-9b66-4c2f-8ccd-b6193f0fb4ce`, reviewer `db1906b7-671b-43a0-8a16-3400eceb17bd`
- Task 2: complete (c8881cd..5c858a0, review clean) — implementer `525d6e73-d2fb-421e-a6fd-a7e1812bf1c3`, reviewer `082e6a7b-a649-4c1b-8588-ac2f5a21b0a4`
  - Task 2: minor (deferred): `cors.rs` `parse_origins` allows HTAB/leading-trailing ASCII whitespace via `HeaderValue::from_str`; tighten to reject ASCII whitespace.
  - Task 2: minor (deferred): `cors_allows_configured_origin_and_preflight_headers` does not assert `Access-Control-Allow-Headers` on preflight or `Access-Control-Expose-Headers` on GET.
  - Task 2: minor (deferred): `compression.rs` unit tests do not directly assert `NotForContentType::{GRPC, IMAGES, SSE}` bypasses.
- Task 3: complete (5c858a0..df795d7, review clean) — implementer `ff78fd93-b2fc-42a4-86f0-670d496caa7c`, reviewer `3cf204ab-a5cb-4716-a46e-0b97dc00ea05`
  - Task 3: minor (deferred): `prometheus.rs` `resolve_names` should guard against literal `"_total"` counter name producing empty family name.
  - Task 3: minor (deferred): `prometheus.rs` `format_f64` special values (`NaN`, `+Inf`, `-Inf`, `-0.0`) lack direct unit tests.
  - Task 3: minor (deferred): `prometheus.rs` `escape_help` does not escape `"` for OpenMetrics 1.0 ABNF.
  - Task 3: minor (deferred): `prometheus.rs` `write_histogram_bucket_line` assumes no user attribute named `"le"`.
  - Task 3: minor (deferred): `metrics.rs` `Accept` negotiation checks substring without `q=0` weight parsing.
- Task 4: fix round 1/3 (1 addressed, 0 open — updated `workbench/multi-instance/benches/baseline.rs:718` to `ObservabilityLayer::new()`)
- Task 4: complete (df795d7..b14b29a, review clean) — implementer `f7d7ca1a-fc27-4e3e-b202-7700fcda402e`, reviewer `6313493f-f869-4523-830a-6eeb90c98321`, re-reviewer `4cae2331-5ece-4342-be82-0e40a4a1e543`
  - Task 4: minor (deferred): `layer.rs` allocates `BTreeMap` in `redact_headers` when `log_headers` is true even if `DEBUG` tracing is disabled; guard with `tracing::enabled!(tracing::Level::DEBUG)`.
  - Task 4: minor (deferred): `observability_layer_with_redaction_never_logs_raw_authorization_or_cookie` could also assert non-sensitive header values appear in `all_logged`.
- Task 5: complete (b14b29a..4c04ad0, review clean) — implementer `feaa1c4d-9367-4d16-a9f6-a98496e9e3f4`, reviewer `fd824cfa-8fc6-4974-9c57-62fcb24cd288`
  - Task 5: minor (deferred): emit `#[automatically_derived]` on the generated `impl HasOpenApiSchema` block in `crates/macro/src/openapi_schema.rs:134`.
  - Task 5: minor (deferred): `parse_i64_lit` does not unwrap `Expr::Group` inside `Expr::Unary(UnOp::Neg)`.
  - Task 5: minor (deferred): `tests/compile_fail.rs` globs `tests/ui/*.rs` so the 2 UI tests run under both `compile_fail` and `openapi_schema`.
- Task 6: complete (4c04ad0..d72b2a1, review clean) — implementer `13decd9f-75e5-4b6e-9fec-db05b9192361`, reviewer `19ce2f79-aa5e-4693-ad23-711611757ea2`
  - Task 6: minor (deferred): CSP rustdoc in `docs.rs` should note Swagger UI's inline `<script>` needs `'unsafe-inline'`, nonce, or hash even when self-hosted (whereas Scalar works with `'self'`).
  - Task 6: minor (deferred): `openapi_docs_escapes_title_and_spec_url_against_html_injection` could also positively assert `\u003c/script\u003e` and U+2028/U+2029 escaping.
- Task 7: complete (d72b2a1..3387b4c, review clean) — implementer `f68dd38b-fbfd-49e2-8669-1089a7da45f9`, reviewer `9554e432-1ff3-4697-b81b-3e5693232d28`
  - Task 7: minor (deferred): `crates/app/src/app/builder/mod.rs:487-494` `Why:` comment in `run_inspect_mode` can also mention invalid CORS configs.
  - Task 7: flake-watch: `rume_observability::layer::tests::observability_layer_with_redaction_never_logs_raw_authorization_or_cookie` had a one-off `tracing-core` callsite-interest flake (`span_name: None`) under parallel test execution; investigate & harden in whole-plan review.
- Whole-plan final review (`b7c2349..3387b4c`, reviewer `1493177e-fbfe-44d9-9317-a39a661dc42c`): `POLISH_RECOMMENDED` (0 blockers; 8 polish items including root-cause `tracing-core` callsite-interest flake fix).
- Final polish: complete (`3387b4c..0bc29f6`, implementer `152925fe-6028-4b54-bc38-08b74f4cc7ae`) — all 8 polish items applied, 5× `rume-observability` flake check + all 6 workspace verification gates green.
- **Plan 005 Status: COMPLETE** (`b7c2349..0bc29f6`).
