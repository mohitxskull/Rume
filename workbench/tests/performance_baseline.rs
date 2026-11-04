#[path = "../benches/baseline.rs"]
mod baseline;

#[tokio::test]
async fn test_rume_production_benchmark_network_and_concurrent_load() {
    let concurrencies = [1, 8, 32, 64];
    let results = baseline::run_full_production_suite(&concurrencies, 500).await;
    baseline::print_suite_report(&results);
}
