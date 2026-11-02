//! Prometheus metrics registry, counters, gauges, and histograms.

use std::collections::BTreeMap;
use std::fmt::Write;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

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

/// Standard HTTP request metrics registry with Prometheus text-format exporter.
#[derive(Debug, Clone, Default)]
pub struct MetricsRegistry {
    requests_total: Arc<RwLock<BTreeMap<(String, u16), u64>>>,
    request_latencies: Arc<RwLock<BTreeMap<String, LatencySummary>>>,
    in_flight: Arc<AtomicI64>,
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

        out
    }
}
