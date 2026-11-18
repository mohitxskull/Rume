//! OpenTelemetry-backed Prometheus metrics collection, registry, and Tower middleware.

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Request, Response};
use opentelemetry::KeyValue;
use opentelemetry::metrics::{
    Counter as OtelCounter, Gauge as OtelGauge, Histogram as OtelHistogram, MeterProvider as _,
    ObservableGauge,
};
use opentelemetry_sdk::metrics::{Aggregation, Instrument, SdkMeterProvider, Stream};
use prometheus::{Encoder, Registry, TextEncoder};
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
/// method (e.g. `GET\x00CUSTOM`) would otherwise create a new Prometheus
/// label series per distinct value. Known methods are tracked individually and
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

/// Atomic counter metric.
#[derive(Debug, Default)]
pub struct Counter {
    value: AtomicU64,
}

impl Counter {
    pub fn inc(&self) {
        self.inc_by(1);
    }

    pub fn inc_by(&self, amount: u64) {
        self.value.fetch_add(amount, Ordering::Relaxed);
    }

    pub fn get(&self) -> u64 {
        self.value.load(Ordering::Relaxed)
    }
}

/// Atomic gauge metric.
#[derive(Debug, Default)]
pub struct Gauge {
    value: AtomicI64,
}

impl Gauge {
    pub fn inc(&self) {
        self.value.fetch_add(1, Ordering::Relaxed);
    }

    pub fn dec(&self) {
        self.value.fetch_sub(1, Ordering::Relaxed);
    }

    pub fn set(&self, val: i64) {
        self.value.store(val, Ordering::Relaxed);
    }

    pub fn get(&self) -> i64 {
        self.value.load(Ordering::Relaxed)
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
/// [`opentelemetry_sdk::metrics::SdkMeterProvider`] and [`opentelemetry_prometheus`].
///
/// Metrics are gathered from the underlying [`prometheus::Registry`] and rendered
/// with [`prometheus::TextEncoder`], so label values are escaped per the Prometheus
/// text exposition specification automatically.
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
    prom_registry: Registry,
    _meter_provider: SdkMeterProvider,
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

impl MetricsRegistry {
    /// Creates a new metrics registry backed by an OpenTelemetry `SdkMeterProvider`
    /// and `opentelemetry-prometheus` exporter.
    pub fn new() -> Self {
        let prom_registry = Registry::new();
        let exporter = opentelemetry_prometheus::exporter()
            .with_registry(prom_registry.clone())
            .scope_info_enabled(false)
            .without_target_info()
            .build()
            .expect("prometheus exporter configuration is valid");

        let meter_provider = SdkMeterProvider::builder()
            .with_reader(exporter)
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
            })
            .build();

        let meter = meter_provider.meter("rume");

        // Why: `opentelemetry-prometheus` appends `_total` to monotonic counters
        // automatically, so OTel counter names omit `_total` to produce
        // `http_requests_total`, `health_checks_total`, and `health_check_failures_total`.
        let requests_total = meter
            .u64_counter("http_requests")
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
            .u64_counter("health_checks")
            .with_description("Total number of health check executions partitioned by check name.")
            .build();

        let health_check_failures_total = meter
            .u64_counter("health_check_failures")
            .with_description(
                "Total number of failed health check executions partitioned by check name.",
            )
            .build();

        Self {
            inner: Arc::new(Inner {
                prom_registry,
                _meter_provider: meter_provider,
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

    /// Renders all registered metrics into the standard Prometheus exposition format.
    pub fn render_prometheus(&self) -> String {
        // Refresh pool metrics via registered exporters before gathering.
        if let Ok(exporters) = self.inner.pool_exporters.read() {
            for exp in exporters.iter() {
                exp.export_metrics(self);
            }
        }

        let encoder = TextEncoder::new();
        let metric_families = self.inner.prom_registry.gather();
        let mut buffer = Vec::new();
        encoder
            .encode(&metric_families, &mut buffer)
            .expect("failed to encode prometheus metrics");
        String::from_utf8(buffer).expect("prometheus metrics are not valid utf-8")
    }
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
    use tower::service_fn;

    #[test]
    fn observability_metrics_registry_records_http_and_pool_metrics_in_prometheus_format() {
        let registry = MetricsRegistry::new();

        let initial = registry.render_prometheus();
        assert!(initial.contains("http_requests_in_flight 0"));
        assert!(!initial.contains("otel_scope_"));
        assert!(!initial.contains("target_info"));

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

        let rendered = registry.render_prometheus();
        assert!(rendered.contains("http_requests_total{method=\"GET\",status=\"200\"} 1"));
        assert!(rendered.contains("http_requests_total{method=\"POST\",status=\"201\"} 1"));
        assert!(rendered.contains("http_request_duration_seconds_count{method=\"GET\"} 1"));
        assert!(
            rendered
                .contains("http_request_duration_seconds_bucket{method=\"GET\",le=\"0.025\"} 1")
        );
        assert!(rendered.contains("db_pool_connections{pool=\"primary\",state=\"active\"} 3"));
        assert!(!rendered.contains("otel_scope_"));
        assert!(!rendered.contains("target_info"));
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

        let rendered = registry.render_prometheus();
        assert!(rendered.contains("db_pool_connections{pool=\"primary\",state=\"active\"} 3"));
        assert!(rendered.contains("db_pool_connections{pool=\"primary\",state=\"idle\"} 7"));
        assert!(rendered.contains("db_pool_connections{pool=\"primary\",state=\"leased\"} 2"));
        assert!(rendered.contains("db_pool_connections{pool=\"primary\",state=\"total\"} 10"));
        assert!(rendered.contains("db_pool_connections{pool=\"primary\",state=\"max\"} 20"));
        assert!(rendered.contains("db_pool_in_flight_transactions{pool=\"primary\"} 1"));
        assert!(rendered.contains("db_pool_is_closed{pool=\"primary\"} 0"));
    }

    #[test]
    fn test_record_health_check() {
        let registry = MetricsRegistry::new();
        registry.record_health_check("redis", HealthStatus::Ok, Duration::from_millis(15));
        registry.record_health_check("db", HealthStatus::Failed, Duration::from_millis(120));

        let rendered = registry.render_prometheus();
        assert!(rendered.contains("health_check_status{name=\"redis\"} 1"));
        assert!(rendered.contains("health_check_status{name=\"db\"} 0"));
        assert!(rendered.contains("health_check_duration_seconds{name=\"redis\"} 0.015"));
        assert!(rendered.contains("health_check_duration_seconds{name=\"db\"} 0.12"));
        assert!(rendered.contains("health_checks_total{name=\"redis\"} 1"));
        assert!(rendered.contains("health_checks_total{name=\"db\"} 1"));
        assert!(rendered.contains("health_check_failures_total{name=\"db\"} 1"));
        assert!(!rendered.contains("health_check_failures_total{name=\"redis\"}"));
    }

    #[test]
    fn test_http_method_cardinality_is_bounded() {
        let registry = MetricsRegistry::new();

        registry.record_http_request("GET", 200, Duration::from_millis(5));
        registry.record_http_request("GET", 200, Duration::from_millis(5));
        registry.record_http_request("GET\x00CUSTOM", 200, Duration::from_millis(5));
        registry.record_http_request("DELETE", 204, Duration::from_millis(2));

        let rendered = registry.render_prometheus();
        assert!(rendered.contains("http_requests_total{method=\"GET\",status=\"200\"} 2"));
        assert!(rendered.contains("http_requests_total{method=\"DELETE\",status=\"204\"} 1"));
        assert!(rendered.contains("http_requests_total{method=\"OTHER\",status=\"200\"} 1"));
        assert!(!rendered.contains("GET\x00CUSTOM"));
    }

    #[test]
    fn test_prometheus_label_values_are_escaped() {
        let registry = MetricsRegistry::new();
        let status = PoolStatus {
            leased_connections: 0,
            active_connections: 1,
            idle_connections: 0,
            total_connections: 1,
            max_connections: 10,
            in_flight_transactions: 0,
            is_closed: false,
        };

        registry.record_db_pool_status(r#"prim"ary"#, &status);
        registry.record_health_check(r#"chk"ec"#, HealthStatus::Ok, Duration::ZERO);

        let rendered = registry.render_prometheus();
        assert!(rendered.contains(r#"db_pool_connections{pool="prim\"ary",state="active"} 1"#));
        assert!(rendered.contains(r#"health_check_status{name="chk\"ec"} 1"#));
        assert!(!rendered.contains(r#"db_pool_connections{pool="prim"ary""#));
        assert!(!rendered.contains(r#"health_check_status{name="chk"ec""#));
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

        let rendered1 = registry.render_prometheus();
        assert!(rendered1.contains("db_pool_connections{pool=\"test_pool\",state=\"active\"} 1"));

        counter.store(4, Ordering::Relaxed);
        let rendered2 = registry.render_prometheus();
        assert!(rendered2.contains("db_pool_connections{pool=\"test_pool\",state=\"active\"} 4"));
    }

    #[tokio::test]
    async fn in_flight_decrements_on_future_cancellation() {
        let registry = MetricsRegistry::new();
        assert_eq!(registry.in_flight(), 0);

        let service = service_fn(|_req: Request<Body>| async {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
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
}
