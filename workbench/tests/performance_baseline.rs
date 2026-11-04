#[path = "../benches/baseline.rs"]
mod baseline;

#[tokio::test]
async fn test_rume_performance_baseline_overhead_under_target() {
    let (raw, rume, pool) = baseline::run_benchmarks(2_500).await;
    baseline::print_benchmark_report(&raw, &rume, &pool);
}
