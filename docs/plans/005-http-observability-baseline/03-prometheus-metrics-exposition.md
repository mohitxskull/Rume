# Task 03 — Prometheus / OpenMetrics `/metrics` Exposition

## Goal

Close the `README.md:18` gap by implementing a zero-dependency Prometheus 0.0.4 and OpenMetrics 1.0 text-format renderer over `MetricsRegistry` / `ResourceMetrics` in `rume-observability::prometheus`, and exposing an ergonomic, open-by-default `/metrics` route helper in `rume-http::routing::metrics`.

## Files touched

- `crates/observability/src/prometheus.rs`
- `crates/http/src/routing/metrics.rs`

## Deliverable

1. **Prometheus / OpenMetrics renderer (`crates/observability/src/prometheus.rs`)**:
   - Pure rendering functions accepting `&MetricsSnapshot` and a convenience `render_registry(&MetricsRegistry, PrometheusFormat) -> String`:
     - `render_prometheus(&MetricsSnapshot) -> String` (Prometheus text exposition 0.0.4).
     - `render_openmetrics(&MetricsSnapshot) -> String` (OpenMetrics 1.0, appending `# EOF\n`).
     - `render_metrics(&MetricsSnapshot, PrometheusFormat) -> String` (dispatches to the requested format).
   - Correctly renders every OTel `AggregatedMetrics` / `MetricData` variant emitted by `MetricsRegistry` (`crates/observability/src/metrics.rs:244-301`):
     - `Sum` (monotonic -> `# TYPE <family> counter`; non-monotonic -> `# TYPE <family> gauge`) for `u64`, `i64`, `f64`.
     - **Counter `_total` family-name convention per format**: `MetricsRegistry` counters already end in `_total` (`http_requests_total`, `health_checks_total`, `health_check_failures_total`). In `PrometheusFormat::Text004`, emit `# HELP <name>` and `# TYPE <name> counter` with sample lines `<name>`. In `PrometheusFormat::OpenMetrics100`, strip trailing `_total` for the `# HELP <family>` and `# TYPE <family> counter` metadata lines while ensuring sample lines carry `<family>_total` (per OpenMetrics 1.0 counter rules).
     - `Gauge` (`# TYPE <name> gauge`) for `u64`, `i64`, `f64`.
     - `Histogram` (`# TYPE <name> histogram`) for `u64`, `f64`: emits **cumulative** `<name>_bucket{...,le="<bound>"}` lines (OTel `bucket_counts()` are per-bucket counts — MUST be accumulated into cumulative totals for Prometheus/OpenMetrics!), `<name>_bucket{...,le="+Inf"}`, `<name>_sum`, and `<name>_count`.
   - Emits `# HELP <name> <escaped_description>` and `# TYPE <name> <type>` once per metric family, in deterministic sorted order (metrics and label keys sorted for reproducible output).
   - Properly escapes metric names (sanitizing non-`[a-zA-Z0-9_:]` chars), `# HELP` descriptions (`\`, `\n`), and label values (`\`, `"`, `\n`), and formats `f64` special values (`+Inf`, `-Inf`, `NaN`).
2. **`/metrics` route helper (`crates/http/src/routing/metrics.rs`)**:
   - `metrics_routes<S>() -> Router<S>` where `MetricsRegistry: FromRef<S>` (mirroring `crates/http/src/routing/health.rs:46` `health_routes<S>()`), mounting `GET /metrics` open by default.
   - `metrics_routes_at<S>(path: &str) -> Router<S>` for custom paths, plus a standalone `metrics_handler` accepting `State<MetricsRegistry>` and `HeaderMap`.
   - Negotiates format via `Accept` header: returns `OPENMETRICS_CONTENT_TYPE` (`application/openmetrics-text; version=1.0.0; charset=utf-8`) with `# EOF\n` when `Accept` contains `application/openmetrics-text`, otherwise returns `PROMETHEUS_CONTENT_TYPE` (`text/plain; version=0.0.4; charset=utf-8`).
   - Sets `Cache-Control: no-store` on `/metrics` responses.

## Constraints honoured

- Zero new dependencies (`rules/85-minimalism.md`): `opentelemetry-prometheus` is deprecated at `0.29` and incompatible with `opentelemetry_sdk = "=0.33.1"`; render directly from `ResourceMetrics` already returned by `MetricsRegistry::snapshot()` (`crates/observability/src/metrics.rs:463`).
- `#![forbid(unsafe_code)]` in both `rume-observability` and `rume-http`.
- Preserve `MetricsLayer` bypass default (`"/metrics"`, `crates/observability/src/metrics.rs:591`) and `MetricsRegistry::export_metrics()` pool exporter refresh (`crates/observability/src/metrics.rs:448-452`).
- Approved decision #2: `/metrics` is open by default and composable with user middleware/listeners.
- `rules/70-code-rationale.md`: document `Why:` on OTel-bucket-to-Prometheus-cumulative conversion and on why `opentelemetry-prometheus` is not used.

## Named tests (bodies omitted)

In `crates/observability/src/prometheus.rs`:
- `prometheus_renders_counters_gauges_and_cumulative_histograms` — records HTTP requests (`GET 200` at 12ms, `GET 200` at 300ms), in-flight count, DB pool status, and health check; asserts `# HELP`, `# TYPE`, counter value `2`, gauge values, cumulative histogram buckets (specifically verifying a bucket at `le="0.5"` includes earlier buckets' counts and `le="+Inf"` equals `_count`), `_sum`, and `_count`.
- `prometheus_escapes_label_values_and_help_text` — verifies `\`, `"`, and `\n` in attribute values and descriptions are escaped per Prometheus text exposition rules.
- `openmetrics_appends_eof_sentinel_and_uses_counter_family_name` — asserts `render_openmetrics` ends with `# EOF\n`, uses `# TYPE http_requests counter` / `# HELP http_requests ...` with sample lines carrying `http_requests_total`, while `render_prometheus` keeps `# TYPE http_requests_total counter` and omits `# EOF\n`.
- `prometheus_output_is_deterministic_across_calls` — renders the same snapshot twice and asserts byte-for-byte equality.

In `crates/http/src/routing/metrics.rs`:
- `metrics_routes_serves_prometheus_text_by_default` — hits `GET /metrics` without `Accept`, asserts `200 OK`, `Content-Type: text/plain; version=0.0.4; charset=utf-8`, `Cache-Control: no-store`, and body contains `http_requests_total`.
- `metrics_routes_negotiates_openmetrics_when_requested` — hits `GET /metrics` with `Accept: application/openmetrics-text; version=1.0.0`, asserts `Content-Type: application/openmetrics-text; version=1.0.0; charset=utf-8` and body ends with `# EOF\n`.
- `metrics_routes_refreshes_registered_pool_exporters_on_scrape` — registers a `DynamicPoolExporter`, hits `GET /metrics`, asserts `db_pool_connections` appears in the scraped payload.

## Verification

```bash
cargo fmt --all --check
cargo clippy -j 2 -p rume-observability -p rume-http --all-targets --all-features -- -D warnings
cargo test -j 2 -p rume-observability --lib --all-features prometheus::
cargo test -j 2 -p rume-http --lib --all-features routing::metrics::
```

Expected: zero warnings; all `prometheus::` and `routing::metrics::` tests pass.

## Commit

*Only when the user asks:*

```text
feat(observability,http): add Prometheus/OpenMetrics renderer and /metrics route helper
```

## Done-when

- [ ] `crates/observability/src/prometheus.rs` renders counters, gauges, and cumulative histograms with proper escaping and deterministic ordering.
- [ ] `crates/http/src/routing/metrics.rs` serves `/metrics` open by default with Prometheus/OpenMetrics content negotiation.
- [ ] All 7 named tests pass and clippy is clean with zero warnings.
