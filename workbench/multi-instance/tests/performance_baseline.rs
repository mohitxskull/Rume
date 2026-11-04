#[path = "../benches/baseline.rs"]
mod baseline;

#[tokio::test]
async fn test_rume_production_benchmark_network_and_concurrent_load() {
    let concurrencies = [1, 8, 32, 64];
    let results = baseline::run_full_production_suite(&concurrencies, 500).await;
    baseline::print_suite_report(&results);

    // 1. HTTP Network Concurrency Acceptance Criteria
    for (c, raw, rume) in &results.concurrency_results {
        assert_eq!(
            raw.errors, 0,
            "raw axum error rate must be 0.0% at concurrency {c}"
        );
        assert_eq!(
            rume.errors, 0,
            "rume pipeline error rate must be 0.0% at concurrency {c}"
        );
        assert!(
            rume.throughput > 100.0,
            "rume throughput ({:.0} req/s) must exceed 100 req/s at concurrency {c}",
            rume.throughput
        );
    }

    // 2. Physical Database Checkout Acceptance Criteria
    assert_eq!(
        results.sqlite_db_stats.errors, 0,
        "sqlite checkout error rate must be 0.0%"
    );
    assert!(
        results.sqlite_db_stats.throughput >= 500.0,
        "sqlite checkout throughput ({:.0} q/s) must exceed 500 q/s",
        results.sqlite_db_stats.throughput
    );

    if let Some(ref pg) = results.postgres_db_stats {
        assert_eq!(
            pg.errors, 0,
            "postgresql checkout error rate must be 0.0%"
        );
        assert!(
            pg.throughput >= 200.0,
            "postgresql checkout throughput ({:.0} q/s) must exceed 200 q/s",
            pg.throughput
        );
    }

    // 3. Competing Outbox Relays Acceptance Criteria
    assert_eq!(
        results.outbox_stats.errors, 0,
        "competing outbox relay error rate must be 0.0% (no lost messages or stuck leases)"
    );
    assert!(
        results.outbox_stats.throughput >= 50.0,
        "competing outbox relay throughput ({:.0} msg/s) must exceed 50 msg/s",
        results.outbox_stats.throughput
    );

    // 4. Durable Job Worker Scheduling & Ack Acceptance Criteria
    assert_eq!(
        results.job_stats.errors, 0,
        "durable worker error rate must be 0.0% (no lost jobs or failed acks)"
    );
    assert!(
        results.job_stats.throughput >= 30.0,
        "durable worker throughput ({:.0} jobs/s) must exceed 30 jobs/s",
        results.job_stats.throughput
    );
}
