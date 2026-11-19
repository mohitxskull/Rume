# Task 08 — W3C Child `TraceContext` Propagation & `ManualReader` Metrics (`RC-08`)

## Goal

Propagate the HTTP server request span's child `TraceContext` (rather than the caller's incoming `span_id`) into `req.extensions()`, span fields, and response `traceparent` headers across both OTel-subscriber and non-OTel configurations, add `event.publish_sync` tracing span + `Why:` parent-linking notes on `EventBus`/`Saga`, migrate `MetricsRegistry` from `PeriodicReader` + `InMemoryMetricExporter` to `ManualReader`, and delete unused `Counter`/`Gauge`.

## Files touched

- `crates/observability/Cargo.toml` (swap `opentelemetry_sdk` `testing` feature for `experimental_metrics_custom_reader`)
- `crates/observability/src/layer.rs` (`ObservabilityService::call`, unit test update)
- `crates/observability/src/metrics.rs` (`ManualReader` + `SharedManualReader`, delete `Counter`/`Gauge`, add `Inner::drop`)
- `crates/observability/src/lib.rs` (remove `Counter`, `Gauge` re-exports)
- `crates/messaging/src/events/bus.rs` (`publish_sync` span + `Why:` comments on `scope_future`)
- `crates/enterprise/src/saga/coordinator.rs` (`Why:` comments on `scope_future`)
- `crates/core/tests/integration/request_identity.rs` (`test_w3c_trace_context_automatic_propagation_across_http_outbox_jobs_and_worker`)
- `crates/core/tests/integration/metrics.rs` (remove `counter_and_gauge_primitives` and `Counter`/`Gauge` imports)

## Deliverable

1. **`crates/observability/src/layer.rs`:**
   - Parse optional valid `incoming_tc` from `TRACEPARENT`/`TRACESTATE`, extract from an explicitly empty root context via `global::get_text_map_propagator(|prop| prop.extract_with_context(&OtelContext::new(), &HeaderMapExtractor(req.headers())))` (never `prop.extract`, which inherits ambient `OtelContext::current()` when headers are absent!), falling back to `incoming_tc.to_otel_context()` when `incoming_tc` is `Some` and the extracted `Context` is invalid, or staying `OtelContext::new()`.
   - Construct the `http_request` span and **unconditionally** call `let _ = span.set_parent(parent_cx);` (passing `OtelContext::new()` when unheadered so `OpenTelemetryLayer::on_new_span` cannot attach an ambient thread-local `OtelContext::current()`) **before** calling `let span_cx = span.context();` (`tracing-opentelemetry` `0.34.0` allocates the span's fresh child/root `span_id` on `set_parent` -> `context()` when `OpenTelemetryLayer` is installed, or returns an invalid `Context` when absent).
   - If `TraceContext::from_otel_context(&span_cx)` yields `Some(mut tc)` (preserving `incoming_tc.tracestate` if `tc.tracestate` is `None`), use `server_tc = tc` and `exec_cx = span_cx`; otherwise (no `OpenTelemetryLayer` active) compute `server_tc = incoming_tc.as_ref().map_or_else(TraceContext::generate, TraceContext::new_child)` and `exec_cx = server_tc.to_otel_context()`.
   - Record `server_tc.traceparent` (and `tracestate`) on `span`, insert `server_tc` into `req.extensions_mut()`, attach `.with_context(exec_cx).instrument(span)`, and write `server_tc.traceparent` (and `tracestate`) on the HTTP response.
   - Update `observability_layer_extracts_w3c_traceparent_into_otel_span_and_response` (`layer.rs:295-329`) and `test_w3c_trace_context_automatic_propagation_across_http_outbox_jobs_and_worker` (`request_identity.rs:420-490`, capturing `handler_tc = TraceContext::current()` inside the handler while keeping `:517` standalone) so assertions check `trace_id == incoming_trace_id`, `span_id != incoming_span_id`, and exact equality across handler `TraceContext::current()`, outbox/job captured `trace_context`, and response `traceparent` header.
2. **`crates/messaging/src/events/bus.rs` & `crates/enterprise/src/saga/coordinator.rs`:**
   - Wrap `EventBus::publish_sync` listener dispatch in an `info_span!("event.publish_sync", event.type = E::TYPE, traceparent = ...)` scoped via `tc.scope_future(span, ...)` when `TraceContext::current()` is present.
   - Add `Why:` comments on `EventBus` and `Saga` (`execute_forward_from_recoverable`, `compensate_completed_recoverable`) explaining why `scope_future` binds `parent_tc` both for `OpenTelemetryLayer` parent linking across spawned tasks / step boundaries and for `OtelContext::current()` fallback when no OTel layer is active.
3. **`crates/observability/src/metrics.rs`, `Cargo.toml`, `lib.rs`:**
   - In `crates/observability/Cargo.toml:21`, replace `features = ["testing"]` with `features = ["experimental_metrics_custom_reader"]` (verified in `opentelemetry_sdk` `0.33.1` to export `ManualReader`, `reader::MetricReader`, and `Pipeline`).
   - Replace `InMemoryMetricExporter` + non-OTLP `PeriodicReader` with `Arc<ManualReader>` wrapped in a private `SharedManualReader(Arc<ManualReader>)` implementing `opentelemetry_sdk::metrics::reader::MetricReader`, passed via `.with_reader(...)`.
   - In `MetricsRegistry::export_metrics()`, refresh pool exporters, allocate `ResourceMetrics::default()`, call `self.inner.manual_reader.collect(&mut rm)`, and return `vec![rm]` on `Ok(())` (zero background threads spawned). Add `Drop for Inner` invoking `meter_provider.shutdown()` (offloading to a helper thread when `feature = "otel"` periodic exporter may be attached to avoid `current_thread` Tokio shutdown deadlock).
   - Delete `pub struct Counter` and `pub struct Gauge` (`metrics.rs:50-92`), remove their re-exports in `crates/observability/src/lib.rs`, and remove `counter_and_gauge_primitives` in `crates/core/tests/integration/metrics.rs`.

## Constraints honoured

- `rules/70-code-rationale.md`:
  - `Why:` comment in `layer.rs` explaining why `extract_with_context(&OtelContext::new(), ...)` + unconditional `span.set_parent(parent_cx)` BEFORE `span.context()` is mandatory (prevents unheadered HTTP requests from inheriting an ambient thread-local `OtelContext::current()` via `extract` or `OpenTelemetryLayer::on_new_span`, and prevents `AlreadyStarted` from blocking `set_parent`) alongside why non-OTel fallback calls `new_child()` + `to_otel_context()`.
  - `Why:` comment in `metrics.rs` explaining why `ManualReader` (`experimental_metrics_custom_reader`) replaces `PeriodicReader` + `InMemoryMetricExporter` (avoids spawning a 60s OS thread per registry, avoids `PeriodicReader::shutdown()` current-thread Tokio deadlocks, and drops `opentelemetry_sdk/testing` from production deps; revisit when `ManualReader` stabilizes without the `experimental_*` feature flag).
- `crates/core/tests/integration/property_tests.rs` (`TraceContext` invariants) and `crates/core/tests/integration/metrics.rs` (`metrics_middleware_and_otel_registry`).

## Named tests (bodies omitted)

- `observability_layer_emits_child_span_id_with_and_without_otel_subscriber` (`crates/observability/src/layer.rs` `#[cfg(test)]`) — tests both (a) incoming `traceparent` with and without an explicit `SdkTracerProvider` + `tracing_opentelemetry::layer()` (asserting `req.extensions::<TraceContext>()`, `TraceContext::current()`, and response `traceparent` share the same child `span_id != incoming_span_id` under `incoming_trace_id`), and (b) an unheadered HTTP request executed while an ambient `OtelContext` is attached (asserting a fresh isolated root `trace_id != ambient_trace_id` is started).
- `event_bus_publish_sync_creates_span_and_propagates_trace_context` (`crates/messaging/src/events/bus.rs` `#[cfg(test)]`) — calls `publish_sync` inside a parent `TraceContext` and verifies listeners observe `TraceContext::current()` with preserved `trace_id`.
- `metrics_registry_collects_without_spawning_periodic_reader_thread` (`crates/observability/src/metrics.rs` `#[cfg(test)]`) — asserts `MetricsRegistry::new()` + `snapshot()` records HTTP counters, `ExplicitBucketHistogram` buckets, `http_requests_in_flight` `ObservableGauge` `0`, and `DynamicPoolExporter` gauges synchronously via `ManualReader` and drops cleanly inside `#[tokio::test]` (current-thread runtime) without hanging.

## Verification

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -j 2 -- -D warnings
cargo test -p rume-observability --all-features -j 2
cargo test -p rume-messaging --all-features -j 2
cargo test -p rume-enterprise --all-features -j 2
cargo test -p rume --test integration --all-features request_identity -j 2
cargo test -p rume --test integration --all-features metrics -j 2
```

Expected: 0 warnings and all observability/messaging/enterprise/integration tests pass.

## Commit

Only when the user asks:

```text
fix(observability): propagate server child TraceContext and switch MetricsRegistry to ManualReader
```

## Done-when

- [ ] `crates/observability/src/layer.rs` inserts, logs, and returns the server span's child `TraceContext` (both with and without `OpenTelemetryLayer`).
- [ ] `crates/observability/Cargo.toml` no longer enables `opentelemetry_sdk/testing`; `metrics.rs` uses `ManualReader` and has no `Counter`, `Gauge`, `InMemoryMetricExporter`, or `PeriodicReader`.
- [ ] `EventBus::publish_sync` creates an `event.publish_sync` span and `EventBus` / `Saga` carry `Why:` comments on `scope_future`.
- [ ] All verification commands exit 0 with 0 warnings.
