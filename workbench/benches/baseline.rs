//! Production Latency & Throughput Benchmark (REL-08)
//!
//! Measures:
//! 1. Raw Axum baseline throughput (req/sec) and latency percentiles (P50, P95, P99).
//! 2. Rume HTTP pipeline throughput (req/sec) and latency percentiles (P50, P95, P99)
//!    with full middleware (observability, W3C trace context, request identity).
//! 3. Framework middleware latency overhead (P50, P95, P99).
//! 4. Persistent connection pool acquisition latency (P50, P95, P99).
//! 5. Validates that Rume's middleware overhead is under 0.15ms (150 µs).

use std::time::{Duration, Instant};
use axum::routing::get;
use axum::Router;
use rume::app::ObservabilityLayer;
use rume::testing::{SqliteTestFixture, TestApp};

#[derive(Debug, Clone)]
pub struct LatencyStats {
    pub count: usize,
    pub elapsed: Duration,
    pub throughput: f64,
    pub p50: Duration,
    pub p95: Duration,
    pub p99: Duration,
    pub min: Duration,
    pub max: Duration,
    pub mean: Duration,
}

impl LatencyStats {
    pub fn compute(mut samples: Vec<Duration>, elapsed: Duration) -> Self {
        assert!(!samples.is_empty());
        samples.sort();
        let count = samples.len();
        let p50 = samples[count * 50 / 100];
        let p95 = samples[count * 95 / 100];
        let p99 = samples[count * 99 / 100];
        let min = samples[0];
        let max = samples[count - 1];
        let total_nanos: u128 = samples.iter().map(|d| d.as_nanos()).sum();
        let mean = Duration::from_nanos((total_nanos / count as u128) as u64);
        let throughput = count as f64 / elapsed.as_secs_f64();
        Self {
            count,
            elapsed,
            throughput,
            p50,
            p95,
            p99,
            min,
            max,
            mean,
        }
    }
}

pub async fn run_benchmarks(iterations: usize) -> (LatencyStats, LatencyStats, LatencyStats) {
    // 1. Raw Axum Baseline
    let raw_router = Router::new().route("/bench", get(|| async { "pong" }));
    let raw_app = TestApp::new(raw_router);

    // Warmup
    for _ in 0..500 {
        let _ = raw_app.get("/bench").await;
    }

    let mut raw_samples = Vec::with_capacity(iterations);
    let start_raw = Instant::now();
    for _ in 0..iterations {
        let t0 = Instant::now();
        let _ = raw_app.get("/bench").await;
        raw_samples.push(t0.elapsed());
    }
    let elapsed_raw = start_raw.elapsed();
    let raw_stats = LatencyStats::compute(raw_samples, elapsed_raw);

    // 2. Rume HTTP Pipeline (with ObservabilityLayer + W3C TraceContext)
    let rume_router = Router::new()
        .route("/bench", get(|| async { "pong" }))
        .layer(ObservabilityLayer::new());
    let rume_app = TestApp::new(rume_router);

    // Warmup
    for _ in 0..500 {
        let _ = rume_app.get("/bench").await;
    }

    let mut rume_samples = Vec::with_capacity(iterations);
    let start_rume = Instant::now();
    for _ in 0..iterations {
        let t0 = Instant::now();
        let _ = rume_app.get("/bench").await;
        rume_samples.push(t0.elapsed());
    }
    let elapsed_rume = start_rume.elapsed();
    let rume_stats = LatencyStats::compute(rume_samples, elapsed_rume);

    // 3. Persistent Database Pool Acquisition Latency
    let fixture = SqliteTestFixture::create()
        .await
        .expect("create sqlite fixture");
    let mut pool_samples = Vec::with_capacity(iterations);
    let start_pool = Instant::now();
    for _ in 0..iterations {
        let t0 = Instant::now();
        let lease = fixture.pool().acquire_lease().expect("acquire lease");
        pool_samples.push(t0.elapsed());
        drop(lease);
    }
    let elapsed_pool = start_pool.elapsed();
    let pool_stats = LatencyStats::compute(pool_samples, elapsed_pool);

    (raw_stats, rume_stats, pool_stats)
}

fn format_dur(d: Duration) -> String {
    let micros = d.as_micros();
    if micros < 1000 {
        format!("{micros} µs")
    } else {
        format!("{:.3} ms", d.as_secs_f64() * 1000.0)
    }
}

pub fn print_benchmark_report(
    raw: &LatencyStats,
    rume: &LatencyStats,
    pool: &LatencyStats,
) {
    let p50_overhead = rume.p50.saturating_sub(raw.p50);
    let p95_overhead = rume.p95.saturating_sub(raw.p95);
    let p99_overhead = rume.p99.saturating_sub(raw.p99);
    let mean_overhead = rume.mean.saturating_sub(raw.mean);

    println!();
    println!("==========================================================================================");
    println!("                          RUME PERFORMANCE BENCHMARK (REL-08)                             ");
    println!("==========================================================================================");
    println!(
        "  Iterations: {} requests per test (elapsed: raw {}, rume {})",
        raw.count,
        format_dur(raw.elapsed),
        format_dur(rume.elapsed)
    );
    println!("------------------------------------------------------------------------------------------");
    println!("  Metric                     Raw Axum          Rume Pipeline     Overhead / Delta        ");
    println!("------------------------------------------------------------------------------------------");
    println!(
        "  Throughput (req/s)         {:>12.0}      {:>12.0}      {:>+10.1}%",
        raw.throughput,
        rume.throughput,
        ((rume.throughput - raw.throughput) / raw.throughput) * 100.0
    );
    println!(
        "  Latency P50                {:>12}      {:>12}      {:>12} (budget: < 150 µs)",
        format_dur(raw.p50),
        format_dur(rume.p50),
        format_dur(p50_overhead)
    );
    println!(
        "  Latency P95                {:>12}      {:>12}      {:>12}",
        format_dur(raw.p95),
        format_dur(rume.p95),
        format_dur(p95_overhead)
    );
    println!(
        "  Latency P99                {:>12}      {:>12}      {:>12}",
        format_dur(raw.p99),
        format_dur(rume.p99),
        format_dur(p99_overhead)
    );
    println!(
        "  Latency Mean               {:>12}      {:>12}      {:>12}",
        format_dur(raw.mean),
        format_dur(rume.mean),
        format_dur(mean_overhead)
    );
    println!("------------------------------------------------------------------------------------------");
    println!("  Database Pool Lease Acquisition ({} operations):", pool.count);
    println!("    Throughput: {:.0} leases/sec", pool.throughput);
    println!("    P50: {}, P95: {}, P99: {}, Min: {}, Max: {}",
        format_dur(pool.p50),
        format_dur(pool.p95),
        format_dur(pool.p99),
        format_dur(pool.min),
        format_dur(pool.max)
    );
    println!("==========================================================================================");

    // Assert conformance to budget
    let target_overhead = Duration::from_micros(150);
    assert!(
        p50_overhead < target_overhead,
        "REL-08 CONFORMANCE FAILURE: P50 latency overhead ({:?}) exceeds 0.15ms target ({:?})",
        p50_overhead,
        target_overhead
    );
    println!("  [PASS] Rume P50 overhead {} conforms to production budget (< 150 µs).", format_dur(p50_overhead));
    println!("==========================================================================================");
    println!();
}

#[allow(dead_code)]
#[tokio::main]
async fn main() {
    let (raw, rume, pool) = run_benchmarks(10_000).await;
    print_benchmark_report(&raw, &rume, &pool);
}
