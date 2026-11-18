# Task 02 — `rume-observability` & Unified OTel Backbone

## Goal

Create `rume-observability` (`crates/observability`) as the unified OTel traces/logs/metrics layer: migrate `MetricsRegistry` to `opentelemetry::metrics` (`SdkMeterProvider`) + `opentelemetry-prometheus`, replace `CURRENT_TRACE_CONTEXT` (`tokio::task_local!`) with `TraceContextPropagator` + `tracing_opentelemetry::OpenTelemetrySpanExt` in `ObservabilityLayer`, and gate `opentelemetry-otlp` behind `feature = "otel"`.

## Files touched

- `Cargo.toml` (`[workspace.members]`: add `"crates/observability"`)
- `crates/observability/Cargo.toml`
- `crates/observability/src/lib.rs`
- `crates/observability/src/otel.rs` (`try_init_tracing`, `init_tracing`, `ObservabilityBuilder`, `ObservabilityGuard`, `SdkTracerProvider`, `SdkMeterProvider`, `SdkLoggerProvider`, `OpenTelemetryLayer`, `OpenTelemetryTracingBridge`, `#[cfg(feature = "otel")]` OTLP exporter wiring)
- `crates/observability/src/trace_context.rs` (`TraceContext`, `TRACEPARENT`, `TRACESTATE` moved here from `app/observability.rs` so `TraceContext::current()` can read `tracing::Span::current().context()` via `TraceContextPropagator` / `OpenTelemetrySpanExt` without orphan-rule conflicts)
- `crates/observability/src/layer.rs` (`ObservabilityLayer`, extracting/injecting `TraceContext` via `opentelemetry::global::get_text_map_propagator` + `OpenTelemetrySpanExt::set_parent`)
- `crates/observability/src/metrics.rs` (moved & migrated from `crates/core/src/metrics/registry.rs` and `crates/core/src/metrics/layer.rs`: `MetricsRegistry`, `MetricsLayer`, `Counter`, `Gauge`, `PoolMetricsExporter`, `DynamicPoolExporter`; `register_pool` moves to `MetricsRegistryPoolExt` in `rume-data` in Task 03)

## Deliverable

1. `crates/observability` (`rume-observability`) depends only on `rume-foundation`, `opentelemetry`, `opentelemetry_sdk`, `opentelemetry-prometheus`, `opentelemetry-appender-tracing`, `tracing-opentelemetry`, `tracing-subscriber`, `axum`, `tower`, `prometheus`, and optionally `opentelemetry-otlp` (`feature = "otel"`).
2. `CURRENT_TRACE_CONTEXT` `tokio::task_local!` is eliminated; `ObservabilityLayer` extracts incoming `traceparent`/`tracestate` into `opentelemetry::Context`, attaches it to the request `tracing::Span` via `OpenTelemetrySpanExt::set_parent`, and injects the active span's `TraceContext` into response headers and request extensions.
3. `TraceContext::current()` reads from the active `tracing::Span::current().context()` via the global `TraceContextPropagator` (falling back cleanly when no OTel subscriber is active).
4. `MetricsRegistry` records HTTP and DB pool metrics via `opentelemetry::metrics::Meter` backed by `SdkMeterProvider` + `opentelemetry-prometheus` configured with `.without_scope_info().without_target_info()`, `SdkMeterProvider` `View` bucket boundaries for `HISTOGRAM_BUCKETS`, and initial zero-observation / `ObservableGauge` handling so `http_requests_in_flight 0` is emitted before any request arrives. `MetricsRegistry::render_prometheus(&self) -> String` preserves the exact method name and Prometheus text exposition format with bounded `http_method_label` (`crates/core/src/metrics/registry.rs:28`).

## Constraints honoured

- `crates/core/src/app/observability.rs`: `try_init_tracing()` yields `Ok(())` or non-panicking `Err` if a global subscriber is already registered; `TraceContext::{is_valid_traceparent, parse_serialized, trace_id, span_id, flags, new_child, generate}` and `FromRequestParts`, `X-Request-ID` CSPRNG UUID v4 generation/propagation, and sanitized logging (no request bodies/credentials) preserved.
- `crates/core/src/metrics/registry.rs`: `http_method_label` cardinality bound (9 known methods + `OTHER`), `HISTOGRAM_BUCKETS`, `in_flight` tracking, `render_prometheus()`, `PoolMetricsExporter` (using `rume_foundation::telemetry::PoolStatus`), and `HealthStatus` metric recording preserved.
- `crates/core/tests/integration/metrics.rs`: `/metrics` endpoint returns `200 OK` with `text/plain; version=0.0.4` Prometheus format and exact `http_requests_in_flight 0`, `http_requests_total`, and `http_request_duration_seconds_count` series.

## Named tests (bodies omitted)

- `observability_trace_context_validates_and_roundtrips_traceparent_and_tracestate` — asserts `TraceContext::is_valid_traceparent` rejects all-zero trace/span IDs and `parse_serialized` roundtrips `traceparent` + `tracestate`.
- `observability_layer_extracts_w3c_traceparent_into_otel_span_and_response` — asserts an incoming `traceparent` + `tracestate` header sets the parent OTel `TraceId` on the active `tracing::Span`, makes `TraceContext::current()` return matching `trace_id`, and echoes `traceparent`/`tracestate` on the HTTP response.
- `observability_metrics_registry_records_http_and_pool_metrics_in_prometheus_format` — asserts `MetricsRegistry::render_prometheus()` outputs `http_requests_in_flight 0` initially, records request count, duration histogram, in-flight gauge, and `DynamicPoolExporter` gauges without `otel_scope_*` or `target_info` noise.
- `observability_try_init_tracing_is_idempotent_and_non_panicking` — asserts repeated `init_tracing()` calls do not panic when a subscriber is already installed.

## Verification

```bash
cargo check -p rume-observability -j 2
cargo check -p rume-observability --features otel -j 2
cargo test -p rume-observability -j 2
```

Expected: `rume-observability` compiles both with default features and with `--features otel` with 0 warnings, and all unit tests pass.

## Commit

Only when the user asks:

```text
feat(observability): add rume-observability with OTel tracing, logs, and prometheus metrics
```

## Done-when

- [ ] `crates/observability/src/lib.rs` has `#![forbid(unsafe_code)]` and depends only on `rume-foundation` internally.
- [ ] `CURRENT_TRACE_CONTEXT` `tokio::task_local!` is not present in `crates/observability`.
- [ ] `opentelemetry-otlp` is gated behind `feature = "otel"`.
- [ ] `cargo test -p rume-observability -j 2` passes with 0 warnings.
