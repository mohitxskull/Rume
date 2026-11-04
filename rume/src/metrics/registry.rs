//! Prometheus metrics registry, counters, gauges, and histograms.

use std::collections::BTreeMap;
use std::fmt::Write;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use crate::persistence::PoolStatus;
use crate::routing::HealthStatus;

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

const HISTOGRAM_BUCKETS: [f64; 11] = [
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];

#[derive(Debug, Clone, Default)]
struct LatencySummary {
    count: u64,
    sum: f64,
    buckets: [u64; 11],
}

#[derive(Debug, Clone, Default)]
struct DbPoolMetrics {
    leased: u32,
    active: u32,
    idle: u32,
    total: u32,
    max: u32,
    in_flight_tx: u32,
    is_closed: bool,
}

#[derive(Debug, Clone)]
struct HealthCheckMetric {
    status: u8,
    duration_secs: f64,
}

/// Standard HTTP request metrics registry with Prometheus text-format exporter.
#[derive(Debug, Clone, Default)]
pub struct MetricsRegistry {
    requests_total: Arc<RwLock<BTreeMap<(String, u16), u64>>>,
    request_latencies: Arc<RwLock<BTreeMap<String, LatencySummary>>>,
    in_flight: Arc<AtomicI64>,
    db_pools: Arc<RwLock<BTreeMap<String, DbPoolMetrics>>>,
    health_checks: Arc<RwLock<BTreeMap<String, HealthCheckMetric>>>,
}

impl MetricsRegistry {
    /// Creates a new empty metrics registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records an HTTP request completion with method, status, and duration.
    pub fn record_http_request(&self, method: &str, status: u16, duration: Duration) {
        if let Ok(mut total) = self.requests_total.write() {
            *total.entry((method.to_uppercase(), status)).or_default() += 1;
        }

        let secs = duration.as_secs_f64();
        if let Ok(mut latencies) = self.request_latencies.write() {
            let summary = latencies.entry(method.to_uppercase()).or_default();
            summary.count += 1;
            summary.sum += secs;
            for (idx, &boundary) in HISTOGRAM_BUCKETS.iter().enumerate() {
                if secs <= boundary {
                    summary.buckets[idx] += 1;
                }
            }
        }
    }

    /// Increments in-flight requests gauge.
    pub fn inc_in_flight(&self) {
        self.in_flight.fetch_add(1, Ordering::Relaxed);
    }

    /// Decrements in-flight requests gauge.
    pub fn dec_in_flight(&self) {
        self.in_flight.fetch_sub(1, Ordering::Relaxed);
    }

    /// Returns current in-flight requests.
    pub fn in_flight(&self) -> i64 {
        self.in_flight.load(Ordering::Relaxed)
    }

    /// Records connection pool metrics for a database pool.
    pub fn record_db_pool_status(&self, pool_name: &str, status: &PoolStatus) {
        if let Ok(mut pools) = self.db_pools.write() {
            pools.insert(
                pool_name.to_string(),
                DbPoolMetrics {
                    leased: status.leased_connections,
                    active: status.active_connections,
                    idle: status.idle_connections,
                    total: status.total_connections,
                    max: status.max_connections,
                    in_flight_tx: status.in_flight_transactions,
                    is_closed: status.is_closed,
                },
            );
        }
    }

    /// Records the execution status and duration of a health check.
    pub fn record_health_check(&self, name: &str, status: HealthStatus, duration: Duration) {
        if let Ok(mut checks) = self.health_checks.write() {
            checks.insert(
                name.to_string(),
                HealthCheckMetric {
                    status: if status == HealthStatus::Ok { 1 } else { 0 },
                    duration_secs: duration.as_secs_f64(),
                },
            );
        }
    }

    /// Renders all registered metrics into the standard Prometheus exposition format.
    pub fn render_prometheus(&self) -> String {
        let mut out = String::new();

        // 1. in_flight gauge
        writeln!(
            out,
            "# HELP http_requests_in_flight Current number of HTTP requests being processed."
        )
        .unwrap();
        writeln!(out, "# TYPE http_requests_in_flight gauge").unwrap();
        writeln!(out, "http_requests_in_flight {}", self.in_flight()).unwrap();

        // 2. requests_total counter
        writeln!(
            out,
            "# HELP http_requests_total Total number of HTTP requests partitioned by status code and method."
        )
        .unwrap();
        writeln!(out, "# TYPE http_requests_total counter").unwrap();
        if let Ok(totals) = self.requests_total.read() {
            for ((method, status), count) in totals.iter() {
                writeln!(
                    out,
                    "http_requests_total{{method=\"{method}\",status=\"{status}\"}} {count}"
                )
                .unwrap();
            }
        }

        // 3. request_duration_seconds summary & histogram
        writeln!(
            out,
            "# HELP http_request_duration_seconds Latency of HTTP requests in seconds."
        )
        .unwrap();
        writeln!(out, "# TYPE http_request_duration_seconds histogram").unwrap();
        if let Ok(latencies) = self.request_latencies.read() {
            for (method, summary) in latencies.iter() {
                for (idx, &boundary) in HISTOGRAM_BUCKETS.iter().enumerate() {
                    let bucket_count = summary.buckets[idx];
                    writeln!(
                        out,
                        "http_request_duration_seconds_bucket{{method=\"{method}\",le=\"{boundary}\"}} {bucket_count}"
                    )
                    .unwrap();
                }
                writeln!(
                    out,
                    "http_request_duration_seconds_bucket{{method=\"{method}\",le=\"+Inf\"}} {}",
                    summary.count
                )
                .unwrap();
                writeln!(
                    out,
                    "http_request_duration_seconds_count{{method=\"{method}\"}} {}",
                    summary.count
                )
                .unwrap();
                writeln!(
                    out,
                    "http_request_duration_seconds_sum{{method=\"{method}\"}} {:.6}",
                    summary.sum
                )
                .unwrap();
            }
        }

        // 4. db_pool metrics
        let pools_guard = self.db_pools.read().ok();
        if let Some(pools) = pools_guard.as_ref().filter(|p| !p.is_empty()) {
            writeln!(
                out,
                "# HELP db_pool_connections Current connection count of the database pool by state."
            )
            .unwrap();
            writeln!(out, "# TYPE db_pool_connections gauge").unwrap();
            for (pool, m) in pools.iter() {
                writeln!(out, "db_pool_connections{{pool=\"{pool}\",state=\"active\"}} {}", m.active).unwrap();
                writeln!(out, "db_pool_connections{{pool=\"{pool}\",state=\"idle\"}} {}", m.idle).unwrap();
                writeln!(out, "db_pool_connections{{pool=\"{pool}\",state=\"leased\"}} {}", m.leased).unwrap();
                writeln!(out, "db_pool_connections{{pool=\"{pool}\",state=\"total\"}} {}", m.total).unwrap();
                writeln!(out, "db_pool_connections{{pool=\"{pool}\",state=\"max\"}} {}", m.max).unwrap();
            }

            writeln!(
                out,
                "# HELP db_pool_in_flight_transactions In-flight database transactions."
            )
            .unwrap();
            writeln!(out, "# TYPE db_pool_in_flight_transactions gauge").unwrap();
            for (pool, m) in pools.iter() {
                writeln!(out, "db_pool_in_flight_transactions{{pool=\"{pool}\"}} {}", m.in_flight_tx).unwrap();
            }

            writeln!(
                out,
                "# HELP db_pool_is_closed Whether the database connection pool is closed."
            )
            .unwrap();
            writeln!(out, "# TYPE db_pool_is_closed gauge").unwrap();
            for (pool, m) in pools.iter() {
                writeln!(out, "db_pool_is_closed{{pool=\"{pool}\"}} {}", if m.is_closed { 1 } else { 0 }).unwrap();
            }
        }

        // 5. health_check metrics
        let checks_guard = self.health_checks.read().ok();
        if let Some(checks) = checks_guard.as_ref().filter(|c| !c.is_empty()) {
            writeln!(
                out,
                "# HELP health_check_status Health check execution status (1 = healthy, 0 = unhealthy)."
            )
            .unwrap();
            writeln!(out, "# TYPE health_check_status gauge").unwrap();
            for (name, m) in checks.iter() {
                writeln!(out, "health_check_status{{name=\"{name}\"}} {}", m.status).unwrap();
            }

            writeln!(
                out,
                "# HELP health_check_duration_seconds Health check execution latency in seconds."
            )
            .unwrap();
            writeln!(out, "# TYPE health_check_duration_seconds gauge").unwrap();
            for (name, m) in checks.iter() {
                writeln!(out, "health_check_duration_seconds{{name=\"{name}\"}} {:.6}", m.duration_secs).unwrap();
            }
        }

        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(rendered.contains("health_check_duration_seconds{name=\"redis\"} 0.015000"));
        assert!(rendered.contains("health_check_duration_seconds{name=\"db\"} 0.120000"));
    }
}
