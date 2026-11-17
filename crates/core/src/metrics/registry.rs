//! Prometheus metrics collection, registry, and Tower middleware.

use std::sync::Arc;
use std::sync::RwLock;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::time::Duration;

use prometheus::{
    Encoder, Gauge as PromGauge, GaugeVec, HistogramOpts, HistogramVec, IntCounterVec, Opts,
    Registry, TextEncoder,
};

use crate::persistence::PoolStatus;
use crate::routing::HealthStatus;

// Histogram buckets for request latency (seconds), matching the legacy summary.
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

/// Standard HTTP request metrics registry backed by a [`prometheus::Registry`].
///
/// Metrics are gathered and rendered with [`prometheus::Encoder::encode`]
/// (`TextEncoder`), so label values are escaped per the Prometheus text
/// exposition specification automatically — no manual interpolation.
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
    registry: Registry,
    requests_total: IntCounterVec,
    request_duration: HistogramVec,
    in_flight: PromGauge,
    db_pool_connections: GaugeVec,
    db_pool_in_flight_transactions: GaugeVec,
    db_pool_is_closed: GaugeVec,
    health_check_status: GaugeVec,
    health_check_duration: GaugeVec,
    health_checks_total: IntCounterVec,
    health_check_failures_total: IntCounterVec,
    pool_exporters: Arc<RwLock<Vec<Arc<dyn PoolMetricsExporter>>>>,
}

impl MetricsRegistry {
    /// Creates a new empty metrics registry.
    pub fn new() -> Self {
        let registry = Registry::new();

        let requests_total = IntCounterVec::new(
            Opts::new(
                "http_requests_total",
                "Total number of HTTP requests partitioned by status code and method.",
            ),
            &["method", "status"],
        )
        .expect("http_requests_total metric definition is valid");

        let request_duration = HistogramVec::new(
            HistogramOpts::new(
                "http_request_duration_seconds",
                "Latency of HTTP requests in seconds.",
            )
            .buckets(HISTOGRAM_BUCKETS.to_vec()),
            &["method"],
        )
        .expect("http_request_duration_seconds metric definition is valid");

        let in_flight = PromGauge::new(
            "http_requests_in_flight",
            "Current number of HTTP requests being processed.",
        )
        .expect("http_requests_in_flight metric definition is valid");

        let db_pool_connections = GaugeVec::new(
            Opts::new(
                "db_pool_connections",
                "Current connection count of the database pool by state.",
            ),
            &["pool", "state"],
        )
        .expect("db_pool_connections metric definition is valid");

        let db_pool_in_flight_transactions = GaugeVec::new(
            Opts::new(
                "db_pool_in_flight_transactions",
                "In-flight database transactions.",
            ),
            &["pool"],
        )
        .expect("db_pool_in_flight_transactions metric definition is valid");

        let db_pool_is_closed = GaugeVec::new(
            Opts::new(
                "db_pool_is_closed",
                "Whether the database connection pool is closed.",
            ),
            &["pool"],
        )
        .expect("db_pool_is_closed metric definition is valid");

        let health_check_status = GaugeVec::new(
            Opts::new(
                "health_check_status",
                "Health check execution status (1 = healthy, 0 = unhealthy).",
            ),
            &["name"],
        )
        .expect("health_check_status metric definition is valid");

        let health_check_duration = GaugeVec::new(
            Opts::new(
                "health_check_duration_seconds",
                "Health check execution latency in seconds.",
            ),
            &["name"],
        )
        .expect("health_check_duration_seconds metric definition is valid");

        let health_checks_total = IntCounterVec::new(
            Opts::new(
                "health_checks_total",
                "Total number of health check executions partitioned by check name.",
            ),
            &["name"],
        )
        .expect("health_checks_total metric definition is valid");

        let health_check_failures_total = IntCounterVec::new(
            Opts::new(
                "health_check_failures_total",
                "Total number of failed health check executions partitioned by check name.",
            ),
            &["name"],
        )
        .expect("health_check_failures_total metric definition is valid");

        // Register every collector with the registry while retaining the
        // handles used to record observations.
        let collectors: Vec<Box<dyn prometheus::core::Collector>> = vec![
            Box::new(requests_total.clone()),
            Box::new(request_duration.clone()),
            Box::new(in_flight.clone()),
            Box::new(db_pool_connections.clone()),
            Box::new(db_pool_in_flight_transactions.clone()),
            Box::new(db_pool_is_closed.clone()),
            Box::new(health_check_status.clone()),
            Box::new(health_check_duration.clone()),
            Box::new(health_checks_total.clone()),
            Box::new(health_check_failures_total.clone()),
        ];
        for collector in collectors {
            registry
                .register(collector)
                .expect("metric registration is valid");
        }

        Self {
            inner: Arc::new(Inner {
                registry,
                requests_total,
                request_duration,
                in_flight,
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

        self.inner
            .requests_total
            .with_label_values(&[method, status.as_str()])
            .inc();

        let secs = duration.as_secs_f64();
        self.inner
            .request_duration
            .with_label_values(&[method])
            .observe(secs);
    }

    /// Increments in-flight requests gauge.
    pub fn inc_in_flight(&self) {
        self.inner.in_flight.inc();
    }

    /// Decrements in-flight requests gauge.
    pub fn dec_in_flight(&self) {
        self.inner.in_flight.dec();
    }

    /// Returns current in-flight requests.
    pub fn in_flight(&self) -> i64 {
        self.inner.in_flight.get() as i64
    }

    /// Registers a custom pool metrics exporter.
    pub fn register_pool_exporter(&self, exporter: Arc<dyn PoolMetricsExporter>) {
        if let Ok(mut exporters) = self.inner.pool_exporters.write() {
            exporters.push(exporter);
        }
    }

    /// Registers a database pool for automated Prometheus telemetry polling.
    pub fn register_pool<B: crate::persistence::DatabaseBackend + 'static>(
        &self,
        name: impl Into<String>,
        pool: &crate::persistence::DatabasePool<B>,
    ) {
        let p = pool.clone();
        let exporter = Arc::new(DynamicPoolExporter::new(name, move || p.status()));
        self.register_pool_exporter(exporter);
    }

    /// Records connection pool metrics for a database pool.
    pub fn record_db_pool_status(&self, pool_name: &str, status: &PoolStatus) {
        let pool = pool_name;
        self.inner
            .db_pool_connections
            .with_label_values(&[pool, "active"])
            .set(status.active_connections as f64);
        self.inner
            .db_pool_connections
            .with_label_values(&[pool, "idle"])
            .set(status.idle_connections as f64);
        self.inner
            .db_pool_connections
            .with_label_values(&[pool, "leased"])
            .set(status.leased_connections as f64);
        self.inner
            .db_pool_connections
            .with_label_values(&[pool, "total"])
            .set(status.total_connections as f64);
        self.inner
            .db_pool_connections
            .with_label_values(&[pool, "max"])
            .set(status.max_connections as f64);

        self.inner
            .db_pool_in_flight_transactions
            .with_label_values(&[pool])
            .set(status.in_flight_transactions as f64);

        self.inner
            .db_pool_is_closed
            .with_label_values(&[pool])
            .set(if status.is_closed { 1.0 } else { 0.0 });
    }

    /// Records the execution status and duration of a health check.
    pub fn record_health_check(&self, name: &str, status: HealthStatus, duration: Duration) {
        let status_val = if status == HealthStatus::Ok { 1.0 } else { 0.0 };
        self.inner
            .health_check_status
            .with_label_values(&[name])
            .set(status_val);
        self.inner
            .health_check_duration
            .with_label_values(&[name])
            .set(duration.as_secs_f64());
        self.inner
            .health_checks_total
            .with_label_values(&[name])
            .inc();
        if status != HealthStatus::Ok {
            self.inner
                .health_check_failures_total
                .with_label_values(&[name])
                .inc();
        }
    }

    /// Renders all registered metrics into the standard Prometheus exposition format.
    ///
    /// Label escaping is performed by [`prometheus::Encoder::encode`]
    /// (`TextEncoder`) per the text exposition specification, so label values
    /// containing backslashes, double quotes, or newlines cannot break the
    /// output or inject labels.
    pub fn render_prometheus(&self) -> String {
        // Refresh pool metrics via registered exporters before gathering
        if let Ok(exporters) = self.inner.pool_exporters.read() {
            for exp in exporters.iter() {
                exp.export_metrics(self);
            }
        }

        let encoder = TextEncoder::new();
        let metric_families = self.inner.registry.gather();
        let mut buffer = Vec::new();
        encoder
            .encode(&metric_families, &mut buffer)
            .expect("failed to encode prometheus metrics");
        String::from_utf8(buffer).expect("prometheus metrics are not valid utf-8")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

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
        // Fuzzed / extended method: must not create an unbounded label series.
        registry.record_http_request("GET\x00CUSTOM", 200, Duration::from_millis(5));
        registry.record_http_request("DELETE", 204, Duration::from_millis(2));

        let rendered = registry.render_prometheus();

        // Known methods are tracked individually...
        assert!(rendered.contains("http_requests_total{method=\"GET\",status=\"200\"} 2"));
        assert!(rendered.contains("http_requests_total{method=\"DELETE\",status=\"204\"} 1"));
        // ...and everything else collapses to a single `OTHER` bucket.
        assert!(rendered.contains("http_requests_total{method=\"OTHER\",status=\"200\"} 1"));
        // The raw fuzzed value must never appear verbatim in a label.
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

        // Label values containing a double-quote and a backslash.
        registry.record_db_pool_status(r#"prim"ary"#, &status);
        registry.record_health_check(r#"chk"ec"#, HealthStatus::Ok, Duration::ZERO);

        let rendered = registry.render_prometheus();

        // Encoder-escaped label values.
        assert!(rendered.contains(r#"db_pool_connections{pool="prim\"ary",state="active"} 1"#));
        assert!(rendered.contains(r#"health_check_status{name="chk\"ec"} 1"#));

        // Raw, unescaped label values must not break out of the quoted string.
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
}
