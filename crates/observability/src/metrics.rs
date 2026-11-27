//! OpenTelemetry metrics collection, registry, and Tower middleware.

use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex, RwLock, Weak};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Request, Response};
use opentelemetry::KeyValue;
use opentelemetry::metrics::{
    Counter as OtelCounter, Gauge as OtelGauge, Histogram as OtelHistogram, MeterProvider as _,
    ObservableGauge,
};
use opentelemetry_sdk::error::OTelSdkResult;
use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData, ResourceMetrics};
use opentelemetry_sdk::metrics::reader::MetricReader;
use opentelemetry_sdk::metrics::{
    Aggregation, Instrument, InstrumentKind, ManualReader, Pipeline, SdkMeterProvider, Stream,
    Temporality,
};
use rume_foundation::health::{HealthCheckObserver, HealthStatus};
use rume_foundation::telemetry::PoolStatus;
use tower::{Layer, Service};

/// Histogram buckets for request latency (seconds), matching the legacy summary.
const HISTOGRAM_BUCKETS: [f64; 11] = [
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];

/// Map an HTTP method to a bounded label value.
///
/// Why: raw HTTP methods are unbounded client input. A fuzzed or extended
/// method (e.g. `GET\x00CUSTOM`) would otherwise create a new metric
/// attribute series per distinct value. Known methods are tracked individually and
/// everything else is collapsed to `OTHER`, keeping the label set bounded at
/// the nine known methods plus `OTHER`.
fn http_method_label(method: &str) -> &'static str {
    match method.to_ascii_uppercase().as_str() {
        "GET" => "GET",
        "POST" => "POST",
        "PUT" => "PUT",
        "DELETE" => "DELETE",
        "HEAD" => "HEAD",
        "OPTIONS" => "OPTIONS",
        "PATCH" => "PATCH",
        "CONNECT" => "CONNECT",
        "TRACE" => "TRACE",
        _ => "OTHER",
    }
}

// Why: `ManualReader` (`experimental_metrics_custom_reader`) replaces the periodic reader and
// in-memory testing exporter so `MetricsRegistry` neither spawns a 60s background OS thread per
// registry nor risks shutdown deadlocks inside a single-threaded Tokio runtime, while removing
// `opentelemetry_sdk`'s `testing` feature from production dependencies. Revisit when
// `ManualReader` stabilizes without the `experimental_*` feature flag.
#[derive(Debug)]
struct SharedManualReader(Arc<ManualReader>);

impl MetricReader for SharedManualReader {
    fn register_pipeline(&self, pipeline: Weak<Pipeline>) {
        self.0.register_pipeline(pipeline);
    }

    fn collect(&self, rm: &mut ResourceMetrics) -> OTelSdkResult {
        self.0.collect(rm)
    }

    fn force_flush(&self) -> OTelSdkResult {
        self.0.force_flush()
    }

    fn shutdown_with_timeout(&self, timeout: Duration) -> OTelSdkResult {
        self.0.shutdown_with_timeout(timeout)
    }

    fn temporality(&self, kind: InstrumentKind) -> Temporality {
        self.0.temporality(kind)
    }
}

/// Trait for types that can export database connection pool metrics into [`MetricsRegistry`].
pub trait PoolMetricsExporter: Send + Sync + std::fmt::Debug {
    /// Returns the logical pool name (e.g. "primary", "analytics").
    fn pool_name(&self) -> &str;
    /// Exports the current pool telemetry into the registry.
    fn export_metrics(&self, registry: &MetricsRegistry);
}

/// Dynamic pool exporter invoking a status closure.
pub struct DynamicPoolExporter<F> {
    name: String,
    status_fn: F,
}

impl<F> DynamicPoolExporter<F>
where
    F: Fn() -> PoolStatus + Send + Sync + 'static,
{
    pub fn new(name: impl Into<String>, status_fn: F) -> Self {
        Self {
            name: name.into(),
            status_fn,
        }
    }
}

impl<F> std::fmt::Debug for DynamicPoolExporter<F> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DynamicPoolExporter")
            .field("name", &self.name)
            .finish()
    }
}

impl<F> PoolMetricsExporter for DynamicPoolExporter<F>
where
    F: Fn() -> PoolStatus + Send + Sync + 'static,
{
    fn pool_name(&self) -> &str {
        &self.name
    }

    fn export_metrics(&self, registry: &MetricsRegistry) {
        let status = (self.status_fn)();
        registry.record_db_pool_status(&self.name, &status);
    }
}

/// Standard HTTP, database pool, and health check metrics registry backed by
/// [`opentelemetry_sdk::metrics::SdkMeterProvider`].
///
/// Metrics are collected on demand via [`ManualReader`] for inspection through
/// [`MetricsRegistry::export_metrics`] and [`MetricsRegistry::snapshot`], and
/// optionally exported over OTLP when `feature = "otel"` is enabled and
/// `OTEL_EXPORTER_OTLP_ENDPOINT` is configured.
#[derive(Clone)]
pub struct MetricsRegistry {
    inner: Arc<Inner>,
}

impl Default for MetricsRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for MetricsRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MetricsRegistry")
            .field("in_flight", &self.in_flight())
            .finish_non_exhaustive()
    }
}

struct Inner {
    manual_reader: Arc<ManualReader>,
    meter_provider: SdkMeterProvider,
    shut_down: AtomicBool,
    #[cfg(feature = "otel")]
    has_otlp_exporter: bool,
    export_lock: Mutex<()>,
    requests_total: OtelCounter<u64>,
    request_duration: OtelHistogram<f64>,
    in_flight_count: Arc<AtomicI64>,
    _in_flight_gauge: ObservableGauge<i64>,
    db_pool_connections: OtelGauge<f64>,
    db_pool_in_flight_transactions: OtelGauge<f64>,
    db_pool_is_closed: OtelGauge<f64>,
    health_check_status: OtelGauge<f64>,
    health_check_duration: OtelGauge<f64>,
    health_checks_total: OtelCounter<u64>,
    health_check_failures_total: OtelCounter<u64>,
    pool_exporters: Arc<RwLock<Vec<Arc<dyn PoolMetricsExporter>>>>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        if self.shut_down.swap(true, Ordering::AcqRel) {
            return;
        }
        // Why: shut down `meter_provider` explicitly on drop so attached readers release
        // resources cleanly. When `feature = "otel"` attaches a background OTLP periodic
        // exporter and drop happens inside a Tokio runtime, offload shutdown to an OS thread so
        // it cannot deadlock a `current_thread` runtime. Callers that need guaranteed OTLP
        // delivery before process exit must await `MetricsRegistry::shutdown()` before teardown.
        #[cfg(feature = "otel")]
        if self.has_otlp_exporter && tokio::runtime::Handle::try_current().is_ok() {
            let provider = self.meter_provider.clone();
            std::thread::spawn(move || {
                let _ = provider.shutdown();
            });
            return;
        }
        let _ = self.meter_provider.shutdown();
    }
}

impl MetricsRegistry {
    /// Creates a new metrics registry backed by an OpenTelemetry `SdkMeterProvider`
    /// with a [`ManualReader`] (and OTLP periodic exporter when `feature = "otel"`
    /// is enabled and `OTEL_EXPORTER_OTLP_ENDPOINT` is set).
    pub fn new() -> Self {
        let manual_reader = Arc::new(ManualReader::builder().build());
        let builder = SdkMeterProvider::builder()
            .with_reader(SharedManualReader(Arc::clone(&manual_reader)))
            .with_view(|instrument: &Instrument| {
                if instrument.name() == "http_request_duration_seconds" {
                    Stream::builder()
                        .with_aggregation(Aggregation::ExplicitBucketHistogram {
                            boundaries: HISTOGRAM_BUCKETS.to_vec(),
                            record_min_max: true,
                        })
                        .build()
                        .ok()
                } else {
                    None
                }
            });

        #[cfg(feature = "otel")]
        let (builder, has_otlp_exporter) = {
            use opentelemetry_otlp::{Protocol, WithExportConfig};
            let mut builder = builder;
            let mut attached = false;
            if let Ok(endpoint) = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT")
                && !endpoint.trim().is_empty()
                && let Ok(otlp_exporter) = opentelemetry_otlp::MetricExporter::builder()
                    .with_http()
                    .with_protocol(Protocol::HttpBinary)
                    .with_endpoint(crate::otel::normalize_otlp_http_endpoint(
                        &endpoint,
                        "/v1/metrics",
                    ))
                    .build()
            {
                builder = builder.with_periodic_exporter(otlp_exporter);
                attached = true;
            }
            (builder, attached)
        };

        let meter_provider = builder.build();
        let meter = meter_provider.meter("rume");

        let requests_total = meter
            .u64_counter("http_requests_total")
            .with_description(
                "Total number of HTTP requests partitioned by status code and method.",
            )
            .build();

        let request_duration = meter
            .f64_histogram("http_request_duration_seconds")
            .with_description("Latency of HTTP requests in seconds.")
            .build();

        let in_flight_count = Arc::new(AtomicI64::new(0));
        let in_flight_ref = Arc::clone(&in_flight_count);
        let in_flight_gauge = meter
            .i64_observable_gauge("http_requests_in_flight")
            .with_description("Current number of HTTP requests being processed.")
            .with_callback(move |observer| {
                observer.observe(in_flight_ref.load(Ordering::Relaxed), &[]);
            })
            .build();

        let db_pool_connections = meter
            .f64_gauge("db_pool_connections")
            .with_description("Current connection count of the database pool by state.")
            .build();

        let db_pool_in_flight_transactions = meter
            .f64_gauge("db_pool_in_flight_transactions")
            .with_description("In-flight database transactions.")
            .build();

        let db_pool_is_closed = meter
            .f64_gauge("db_pool_is_closed")
            .with_description("Whether the database connection pool is closed.")
            .build();

        let health_check_status = meter
            .f64_gauge("health_check_status")
            .with_description("Health check execution status (1 = healthy, 0 = unhealthy).")
            .build();

        let health_check_duration = meter
            .f64_gauge("health_check_duration_seconds")
            .with_description("Health check execution latency in seconds.")
            .build();

        let health_checks_total = meter
            .u64_counter("health_checks_total")
            .with_description("Total number of health check executions partitioned by check name.")
            .build();

        let health_check_failures_total = meter
            .u64_counter("health_check_failures_total")
            .with_description(
                "Total number of failed health check executions partitioned by check name.",
            )
            .build();

        Self {
            inner: Arc::new(Inner {
                manual_reader,
                meter_provider,
                shut_down: AtomicBool::new(false),
                #[cfg(feature = "otel")]
                has_otlp_exporter,
                export_lock: Mutex::new(()),
                requests_total,
                request_duration,
                in_flight_count,
                _in_flight_gauge: in_flight_gauge,
                db_pool_connections,
                db_pool_in_flight_transactions,
                db_pool_is_closed,
                health_check_status,
                health_check_duration,
                health_checks_total,
                health_check_failures_total,
                pool_exporters: Arc::new(RwLock::new(Vec::new())),
            }),
        }
    }

    /// Shuts down the underlying OpenTelemetry `SdkMeterProvider`, flushing any
    /// pending OTLP metrics on a dedicated OS thread so awaiting this method
    /// before runtime teardown cannot deadlock a `current_thread` Tokio runtime
    /// and ensures final OTLP exports complete before process exit.
    ///
    /// Subsequent calls (including [`Drop`]) are no-ops.
    pub async fn shutdown(&self) -> OTelSdkResult {
        if self.inner.shut_down.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        let provider = self.inner.meter_provider.clone();
        let (tx, rx) = tokio::sync::oneshot::channel();
        std::thread::spawn(move || {
            let _ = tx.send(provider.shutdown());
        });
        match rx.await {
            Ok(res) => res,
            Err(_) => Ok(()),
        }
    }

    /// Records an HTTP request completion with method, status, and duration.
    ///
    /// The method label is cardinality-bounded via [`http_method_label`]:
    /// known methods are tracked individually and everything else is reported
    /// under `OTHER`.
    pub fn record_http_request(&self, method: &str, status: u16, duration: Duration) {
        let method = http_method_label(method);
        let status = status.to_string();

        self.inner.requests_total.add(
            1,
            &[
                KeyValue::new("method", method),
                KeyValue::new("status", status),
            ],
        );

        let secs = duration.as_secs_f64();
        self.inner
            .request_duration
            .record(secs, &[KeyValue::new("method", method)]);
    }

    /// Increments in-flight requests gauge.
    pub fn inc_in_flight(&self) {
        self.inner.in_flight_count.fetch_add(1, Ordering::Relaxed);
    }

    /// Decrements in-flight requests gauge.
    pub fn dec_in_flight(&self) {
        self.inner.in_flight_count.fetch_sub(1, Ordering::Relaxed);
    }

    /// Returns current in-flight requests.
    pub fn in_flight(&self) -> i64 {
        self.inner.in_flight_count.load(Ordering::Relaxed)
    }

    /// Registers a custom pool metrics exporter.
    pub fn register_pool_exporter(&self, exporter: Arc<dyn PoolMetricsExporter>) {
        if let Ok(mut exporters) = self.inner.pool_exporters.write() {
            exporters.push(exporter);
        }
    }

    /// Records connection pool metrics for a database pool.
    pub fn record_db_pool_status(&self, pool_name: &str, status: &PoolStatus) {
        let pool = pool_name.to_string();
        let states = [
            ("active", status.active_connections as f64),
            ("idle", status.idle_connections as f64),
            ("leased", status.leased_connections as f64),
            ("total", status.total_connections as f64),
            ("max", status.max_connections as f64),
        ];
        for (state, val) in states {
            self.inner.db_pool_connections.record(
                val,
                &[
                    KeyValue::new("pool", pool.clone()),
                    KeyValue::new("state", state),
                ],
            );
        }

        self.inner.db_pool_in_flight_transactions.record(
            status.in_flight_transactions as f64,
            &[KeyValue::new("pool", pool.clone())],
        );

        self.inner.db_pool_is_closed.record(
            if status.is_closed { 1.0 } else { 0.0 },
            &[KeyValue::new("pool", pool)],
        );
    }

    /// Records the execution status and duration of a health check.
    pub fn record_health_check(&self, name: &str, status: HealthStatus, duration: Duration) {
        let status_val = if status == HealthStatus::Ok { 1.0 } else { 0.0 };
        let attrs = [KeyValue::new("name", name.to_string())];
        self.inner.health_check_status.record(status_val, &attrs);
        self.inner
            .health_check_duration
            .record(duration.as_secs_f64(), &attrs);
        self.inner.health_checks_total.add(1, &attrs);
        if status != HealthStatus::Ok {
            self.inner.health_check_failures_total.add(1, &attrs);
        }
    }

    /// Collects and returns a snapshot of all OpenTelemetry [`ResourceMetrics`]
    /// from the manual reader without spawning background threads.
    pub fn export_metrics(&self) -> Vec<ResourceMetrics> {
        let _guard = self
            .inner
            .export_lock
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        // Refresh pool metrics via registered exporters before collecting.
        if let Ok(exporters) = self.inner.pool_exporters.read() {
            for exp in exporters.iter() {
                exp.export_metrics(self);
            }
        }

        let mut rm = ResourceMetrics::default();
        if self.inner.manual_reader.collect(&mut rm).is_ok() {
            vec![rm]
        } else {
            Vec::new()
        }
    }

    /// Returns a queryable [`MetricsSnapshot`] of all current OpenTelemetry metrics.
    pub fn snapshot(&self) -> MetricsSnapshot {
        MetricsSnapshot {
            resource_metrics: self.export_metrics(),
        }
    }
}

/// Snapshot of exported OpenTelemetry [`ResourceMetrics`] with lookup helpers.
#[derive(Debug, Default)]
pub struct MetricsSnapshot {
    resource_metrics: Vec<ResourceMetrics>,
}

impl MetricsSnapshot {
    /// Returns the underlying OpenTelemetry [`ResourceMetrics`].
    pub fn resource_metrics(&self) -> &[ResourceMetrics] {
        &self.resource_metrics
    }

    /// Returns `true` if any exported metric has the given instrument `name`.
    pub fn has_metric(&self, name: &str) -> bool {
        self.find_metrics(name).next().is_some()
    }

    /// Returns the value of a `u64` counter/sum data point matching `name` and `attrs`.
    pub fn counter_u64(&self, name: &str, attrs: &[(&str, &str)]) -> Option<u64> {
        self.find_metrics(name).find_map(|metric| {
            let AggregatedMetrics::U64(MetricData::Sum(sum)) = metric.data() else {
                return None;
            };
            sum.data_points()
                .find(|dp| attrs_match(dp.attributes(), attrs))
                .map(|dp| dp.value())
        })
    }

    /// Returns the value of an `f64` gauge data point matching `name` and `attrs`.
    pub fn gauge_f64(&self, name: &str, attrs: &[(&str, &str)]) -> Option<f64> {
        self.find_metrics(name).find_map(|metric| {
            let AggregatedMetrics::F64(MetricData::Gauge(gauge)) = metric.data() else {
                return None;
            };
            gauge
                .data_points()
                .find(|dp| attrs_match(dp.attributes(), attrs))
                .map(|dp| dp.value())
        })
    }

    /// Returns the value of an `i64` gauge data point matching `name` and `attrs`.
    pub fn gauge_i64(&self, name: &str, attrs: &[(&str, &str)]) -> Option<i64> {
        self.find_metrics(name).find_map(|metric| {
            let AggregatedMetrics::I64(MetricData::Gauge(gauge)) = metric.data() else {
                return None;
            };
            gauge
                .data_points()
                .find(|dp| attrs_match(dp.attributes(), attrs))
                .map(|dp| dp.value())
        })
    }

    /// Returns `(count, sum, bounds, bucket_counts)` for an `f64` histogram data point
    /// matching `name` and `attrs`.
    pub fn histogram_f64(
        &self,
        name: &str,
        attrs: &[(&str, &str)],
    ) -> Option<(u64, f64, Vec<f64>, Vec<u64>)> {
        self.find_metrics(name).find_map(|metric| {
            let AggregatedMetrics::F64(MetricData::Histogram(hist)) = metric.data() else {
                return None;
            };
            hist.data_points()
                .find(|dp| attrs_match(dp.attributes(), attrs))
                .map(|dp| {
                    (
                        dp.count(),
                        dp.sum(),
                        dp.bounds().collect(),
                        dp.bucket_counts().collect(),
                    )
                })
        })
    }

    fn find_metrics<'a>(
        &'a self,
        name: &'a str,
    ) -> impl Iterator<Item = &'a opentelemetry_sdk::metrics::data::Metric> {
        self.resource_metrics
            .iter()
            .flat_map(|rm| rm.scope_metrics())
            .flat_map(|sm| sm.metrics())
            .filter(move |m| m.name() == name)
    }
}

fn attrs_match<'a>(
    mut dp_attrs: impl Iterator<Item = &'a KeyValue>,
    expected: &[(&str, &str)],
) -> bool {
    let collected: Vec<(&str, String)> = dp_attrs
        .by_ref()
        .map(|kv| (kv.key.as_str(), kv.value.as_str().into_owned()))
        .collect();
    expected.iter().all(|(k, v)| {
        collected
            .iter()
            .any(|(ck, cv)| ck == k && cv.as_str() == *v)
    }) && collected.len() == expected.len()
}

impl HealthCheckObserver for MetricsRegistry {
    fn record_health_check(&self, name: &str, status: HealthStatus, duration: Duration) {
        self.record_health_check(name, status, duration);
    }
}

/// Tower layer for recording HTTP request metrics.
#[derive(Debug, Clone)]
pub struct MetricsLayer {
    registry: MetricsRegistry,
    bypass_path: Arc<String>,
}

impl MetricsLayer {
    /// Creates a new metrics layer wrapping the given registry with default bypass path `/metrics`.
    pub fn new(registry: MetricsRegistry) -> Self {
        Self::with_bypass_path(registry, "/metrics")
    }

    /// Creates a metrics layer with a custom bypass path.
    pub fn with_bypass_path(registry: MetricsRegistry, bypass_path: impl Into<String>) -> Self {
        Self {
            registry,
            bypass_path: Arc::new(bypass_path.into()),
        }
    }
}

impl<S> Layer<S> for MetricsLayer {
    type Service = MetricsService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        MetricsService {
            inner,
            registry: self.registry.clone(),
            bypass_path: Arc::clone(&self.bypass_path),
        }
    }
}

struct InFlightGuard(MetricsRegistry);

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.0.dec_in_flight();
    }
}

/// Tower service recording execution metrics.
#[derive(Clone)]
pub struct MetricsService<S> {
    inner: S,
    registry: MetricsRegistry,
    bypass_path: Arc<String>,
}

impl<S> Service<Request<Body>> for MetricsService<S>
where
    S: Service<Request<Body>, Response = Response<Body>> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    type Response = Response<Body>;
    type Error = S::Error;
    type Future = std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>> + Send>,
    >;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<Body>) -> Self::Future {
        let clone = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, clone);

        if req.uri().path() == self.bypass_path.as_str() {
            return Box::pin(async move { inner.call(req).await });
        }

        let registry = self.registry.clone();
        let method = req.method().to_string();
        let start = Instant::now();

        registry.inc_in_flight();
        let guard = InFlightGuard(registry.clone());

        Box::pin(async move {
            let _guard = guard;
            let response = inner.call(req).await;
            let duration = start.elapsed();

            match response {
                Ok(res) => {
                    let status = res.status().as_u16();
                    registry.record_http_request(&method, status, duration);
                    Ok(res)
                }
                Err(err) => {
                    registry.record_http_request(&method, 500, duration);
                    Err(err)
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;
    use tower::service_fn;

    #[test]
    fn observability_metrics_registry_records_http_and_pool_metrics() {
        let registry = MetricsRegistry::new();

        let initial = registry.snapshot();
        assert_eq!(initial.gauge_i64("http_requests_in_flight", &[]), Some(0));

        registry.record_http_request("GET", 200, Duration::from_millis(12));
        registry.record_http_request("POST", 201, Duration::from_millis(30));

        let exporter = Arc::new(DynamicPoolExporter::new("primary", || PoolStatus {
            leased_connections: 2,
            active_connections: 3,
            idle_connections: 7,
            total_connections: 10,
            max_connections: 20,
            in_flight_transactions: 1,
            is_closed: false,
        }));
        registry.register_pool_exporter(exporter);

        let snap = registry.snapshot();
        assert_eq!(
            snap.counter_u64(
                "http_requests_total",
                &[("method", "GET"), ("status", "200")]
            ),
            Some(1)
        );
        assert_eq!(
            snap.counter_u64(
                "http_requests_total",
                &[("method", "POST"), ("status", "201")]
            ),
            Some(1)
        );

        let (count, _sum, bounds, buckets) = snap
            .histogram_f64("http_request_duration_seconds", &[("method", "GET")])
            .expect("histogram recorded for GET");
        assert_eq!(count, 1);
        assert_eq!(bounds, HISTOGRAM_BUCKETS.to_vec());
        // 12ms = 0.012s falls into the (0.01, 0.025] bucket (index 2).
        assert_eq!(buckets[2], 1);

        assert_eq!(
            snap.gauge_f64(
                "db_pool_connections",
                &[("pool", "primary"), ("state", "active")]
            ),
            Some(3.0)
        );
    }

    #[test]
    fn test_record_db_pool_status() {
        let registry = MetricsRegistry::new();
        let status = PoolStatus {
            leased_connections: 2,
            active_connections: 3,
            idle_connections: 7,
            total_connections: 10,
            max_connections: 20,
            in_flight_transactions: 1,
            is_closed: false,
        };
        registry.record_db_pool_status("primary", &status);

        let snap = registry.snapshot();
        assert_eq!(
            snap.gauge_f64(
                "db_pool_connections",
                &[("pool", "primary"), ("state", "active")]
            ),
            Some(3.0)
        );
        assert_eq!(
            snap.gauge_f64(
                "db_pool_connections",
                &[("pool", "primary"), ("state", "idle")]
            ),
            Some(7.0)
        );
        assert_eq!(
            snap.gauge_f64(
                "db_pool_connections",
                &[("pool", "primary"), ("state", "leased")]
            ),
            Some(2.0)
        );
        assert_eq!(
            snap.gauge_f64(
                "db_pool_connections",
                &[("pool", "primary"), ("state", "total")]
            ),
            Some(10.0)
        );
        assert_eq!(
            snap.gauge_f64(
                "db_pool_connections",
                &[("pool", "primary"), ("state", "max")]
            ),
            Some(20.0)
        );
        assert_eq!(
            snap.gauge_f64("db_pool_in_flight_transactions", &[("pool", "primary")]),
            Some(1.0)
        );
        assert_eq!(
            snap.gauge_f64("db_pool_is_closed", &[("pool", "primary")]),
            Some(0.0)
        );
    }

    #[test]
    fn test_record_health_check() {
        let registry = MetricsRegistry::new();
        registry.record_health_check("redis", HealthStatus::Ok, Duration::from_millis(15));
        registry.record_health_check("db", HealthStatus::Failed, Duration::from_millis(120));

        let snap = registry.snapshot();
        assert_eq!(
            snap.gauge_f64("health_check_status", &[("name", "redis")]),
            Some(1.0)
        );
        assert_eq!(
            snap.gauge_f64("health_check_status", &[("name", "db")]),
            Some(0.0)
        );
        assert_eq!(
            snap.gauge_f64("health_check_duration_seconds", &[("name", "redis")]),
            Some(0.015)
        );
        assert_eq!(
            snap.gauge_f64("health_check_duration_seconds", &[("name", "db")]),
            Some(0.12)
        );
        assert_eq!(
            snap.counter_u64("health_checks_total", &[("name", "redis")]),
            Some(1)
        );
        assert_eq!(
            snap.counter_u64("health_checks_total", &[("name", "db")]),
            Some(1)
        );
        assert_eq!(
            snap.counter_u64("health_check_failures_total", &[("name", "db")]),
            Some(1)
        );
        assert_eq!(
            snap.counter_u64("health_check_failures_total", &[("name", "redis")]),
            None
        );
    }

    #[test]
    fn test_http_method_cardinality_is_bounded() {
        let registry = MetricsRegistry::new();

        registry.record_http_request("GET", 200, Duration::from_millis(5));
        registry.record_http_request("GET", 200, Duration::from_millis(5));
        registry.record_http_request("GET\x00CUSTOM", 200, Duration::from_millis(5));
        registry.record_http_request("DELETE", 204, Duration::from_millis(2));

        let snap = registry.snapshot();
        assert_eq!(
            snap.counter_u64(
                "http_requests_total",
                &[("method", "GET"), ("status", "200")]
            ),
            Some(2)
        );
        assert_eq!(
            snap.counter_u64(
                "http_requests_total",
                &[("method", "DELETE"), ("status", "204")]
            ),
            Some(1)
        );
        assert_eq!(
            snap.counter_u64(
                "http_requests_total",
                &[("method", "OTHER"), ("status", "200")]
            ),
            Some(1)
        );
        assert_eq!(
            snap.counter_u64(
                "http_requests_total",
                &[("method", "GET\x00CUSTOM"), ("status", "200")]
            ),
            None
        );
    }

    #[test]
    fn test_dynamic_pool_exporter_auto_refresh() {
        let registry = MetricsRegistry::new();
        let counter = Arc::new(AtomicU64::new(1));
        let c_clone = Arc::clone(&counter);

        let exporter = Arc::new(DynamicPoolExporter::new("test_pool", move || {
            let active = c_clone.load(Ordering::Relaxed) as u32;
            PoolStatus {
                leased_connections: active,
                active_connections: active,
                idle_connections: 5,
                total_connections: 5 + active,
                max_connections: 20,
                in_flight_transactions: 0,
                is_closed: false,
            }
        }));

        registry.register_pool_exporter(exporter);

        let snap1 = registry.snapshot();
        assert_eq!(
            snap1.gauge_f64(
                "db_pool_connections",
                &[("pool", "test_pool"), ("state", "active")]
            ),
            Some(1.0)
        );

        counter.store(4, Ordering::Relaxed);
        let snap2 = registry.snapshot();
        assert_eq!(
            snap2.gauge_f64(
                "db_pool_connections",
                &[("pool", "test_pool"), ("state", "active")]
            ),
            Some(4.0)
        );
    }

    #[tokio::test]
    async fn in_flight_decrements_on_future_cancellation() {
        let registry = MetricsRegistry::new();
        assert_eq!(registry.in_flight(), 0);

        let service = service_fn(|_req: Request<Body>| async {
            std::future::pending::<()>().await;
            Ok::<_, std::convert::Infallible>(Response::new(Body::empty()))
        });

        let mut metrics_service = MetricsService {
            inner: service,
            registry: registry.clone(),
            bypass_path: Arc::new("/metrics".to_string()),
        };

        let req = Request::builder().body(Body::empty()).unwrap();
        let fut = metrics_service.call(req);

        assert_eq!(registry.in_flight(), 1);
        drop(fut);
        assert_eq!(registry.in_flight(), 0);
    }

    #[tokio::test]
    async fn metrics_registry_collects_without_spawning_periodic_reader_thread() {
        let registry = MetricsRegistry::new();

        let initial = registry.snapshot();
        assert_eq!(initial.gauge_i64("http_requests_in_flight", &[]), Some(0));

        let exporter = Arc::new(DynamicPoolExporter::new("manual_pool", || PoolStatus {
            leased_connections: 1,
            active_connections: 2,
            idle_connections: 4,
            total_connections: 6,
            max_connections: 10,
            in_flight_transactions: 0,
            is_closed: false,
        }));
        registry.register_pool_exporter(exporter);

        registry.record_http_request("GET", 200, Duration::from_millis(12));

        let snap = registry.snapshot();
        assert_eq!(
            snap.counter_u64(
                "http_requests_total",
                &[("method", "GET"), ("status", "200")]
            ),
            Some(1)
        );
        let (count, _sum, bounds, buckets) = snap
            .histogram_f64("http_request_duration_seconds", &[("method", "GET")])
            .expect("histogram recorded for GET");
        assert_eq!(count, 1);
        assert_eq!(bounds, HISTOGRAM_BUCKETS.to_vec());
        assert_eq!(buckets[2], 1);
        assert_eq!(snap.gauge_i64("http_requests_in_flight", &[]), Some(0));
        assert_eq!(
            snap.gauge_f64(
                "db_pool_connections",
                &[("pool", "manual_pool"), ("state", "active")]
            ),
            Some(2.0)
        );

        // Must shut down and drop cleanly inside current-thread #[tokio::test] without hanging.
        registry
            .shutdown()
            .await
            .expect("explicit async shutdown must succeed");
        // Idempotent second shutdown call.
        registry
            .shutdown()
            .await
            .expect("second shutdown must be a no-op");
        drop(registry);
    }

    #[cfg(feature = "otel")]
    #[test]
    fn otlp_metrics_endpoint_is_normalized_with_v1_metrics_path() {
        assert_eq!(
            crate::otel::normalize_otlp_http_endpoint("http://collector:4318", "/v1/metrics"),
            "http://collector:4318/v1/metrics"
        );
        assert_eq!(
            crate::otel::normalize_otlp_http_endpoint(
                "http://collector:4318/v1/metrics",
                "/v1/metrics"
            ),
            "http://collector:4318/v1/metrics"
        );
    }
}
