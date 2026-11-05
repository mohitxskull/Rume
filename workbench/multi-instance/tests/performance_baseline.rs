use std::time::Duration;

#[path = "../benches/baseline.rs"]
mod baseline;

#[tokio::test]
async fn test_rume_production_benchmark_network_and_concurrent_load() {
    let concurrencies = [1, 8, 32, 64];
    let results = baseline::run_full_production_suite(&concurrencies, 500).await;
    baseline::print_suite_report(&results);

    // 0. Environment Recording & Integrity Validation
    assert!(!results.environment.os.is_empty(), "OS must be recorded");
    assert!(
        !results.environment.arch.is_empty(),
        "Arch must be recorded"
    );
    assert!(
        results.environment.logical_cores >= 1,
        "Logical cores must be detected"
    );
    assert_eq!(results.environment.postgres_image, "postgres:16-alpine");

    // 1. HTTP Network Concurrency Acceptance Criteria & Latency Gates
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

        // Latency gates per concurrency tier (calibrated for unoptimized dev/debug test profile)
        let (max_p50, max_p95, max_p99) = match *c {
            1 => (
                Duration::from_millis(5),
                Duration::from_millis(15),
                Duration::from_millis(25),
            ),
            8 => (
                Duration::from_millis(15),
                Duration::from_millis(30),
                Duration::from_millis(45),
            ),
            32 => (
                Duration::from_millis(35),
                Duration::from_millis(60),
                Duration::from_millis(80),
            ),
            _ => (
                Duration::from_millis(75),
                Duration::from_millis(120),
                Duration::from_millis(150),
            ),
        };

        assert!(
            rume.p50 <= max_p50,
            "rume P50 ({:?}) must be <= {:?} at concurrency {c}",
            rume.p50,
            max_p50
        );
        assert!(
            rume.p95 <= max_p95,
            "rume P95 ({:?}) must be <= {:?} at concurrency {c}",
            rume.p95,
            max_p95
        );
        assert!(
            rume.p99 <= max_p99,
            "rume P99 ({:?}) must be <= {:?} at concurrency {c}",
            rume.p99,
            max_p99
        );

        // Enforce P50 overhead delta budget between Raw Axum and Rume pipeline
        assert!(
            rume.p50 <= raw.p50 + Duration::from_millis(6),
            "rume P50 overhead ({:?}) must not exceed raw axum P50 ({:?}) + 6ms",
            rume.p50,
            raw.p50
        );
    }

    // 2. Physical Database Checkout Acceptance Criteria & Latency Gates
    assert_eq!(
        results.sqlite_db_stats.errors, 0,
        "sqlite checkout error rate must be 0.0%"
    );
    assert!(
        results.sqlite_db_stats.throughput >= 500.0,
        "sqlite checkout throughput ({:.0} q/s) must exceed 500 q/s",
        results.sqlite_db_stats.throughput
    );
    assert!(
        results.sqlite_db_stats.p50 <= Duration::from_millis(5),
        "sqlite P50 ({:?}) must be <= 5ms",
        results.sqlite_db_stats.p50
    );
    assert!(
        results.sqlite_db_stats.p95 <= Duration::from_millis(20),
        "sqlite P95 ({:?}) must be <= 20ms",
        results.sqlite_db_stats.p95
    );

    if let Some(ref pg) = results.postgres_db_stats {
        assert_eq!(pg.errors, 0, "postgresql checkout error rate must be 0.0%");
        assert!(
            pg.throughput >= 200.0,
            "postgresql checkout throughput ({:.0} q/s) must exceed 200 q/s",
            pg.throughput
        );
        assert!(
            pg.p50 <= Duration::from_millis(25),
            "postgresql P50 ({:?}) must be <= 25ms",
            pg.p50
        );
        assert!(
            pg.p95 <= Duration::from_millis(60),
            "postgresql P95 ({:?}) must be <= 60ms",
            pg.p95
        );
        assert!(
            pg.p99 <= Duration::from_millis(200),
            "postgresql P99 ({:?}) must be <= 200ms",
            pg.p99
        );
    }

    // 3. Competing Outbox Relays Acceptance Criteria & Latency Gates
    assert_eq!(
        results.outbox_stats.errors, 0,
        "competing outbox relay error rate must be 0.0% (no lost messages or stuck leases)"
    );
    assert!(
        results.outbox_stats.throughput >= 50.0,
        "competing outbox relay throughput ({:.0} msg/s) must exceed 50 msg/s",
        results.outbox_stats.throughput
    );
    assert!(
        results.outbox_stats.p50 <= Duration::from_millis(25),
        "outbox P50 ({:?}) must be <= 25ms",
        results.outbox_stats.p50
    );
    assert!(
        results.outbox_stats.p95 <= Duration::from_millis(65),
        "outbox P95 ({:?}) must be <= 65ms",
        results.outbox_stats.p95
    );
    assert!(
        results.outbox_stats.p99 <= Duration::from_millis(100),
        "outbox P99 ({:?}) must be <= 100ms",
        results.outbox_stats.p99
    );

    // 4. Durable Job Queue Concurrent Claim & Ack Acceptance Criteria & Latency Gates
    assert_eq!(
        results.job_stats.errors, 0,
        "durable queue error rate must be 0.0% (no lost jobs or failed acks)"
    );
    assert!(
        results.job_stats.throughput >= 30.0,
        "durable queue claim/ack throughput ({:.0} jobs/s) must exceed 30 jobs/s",
        results.job_stats.throughput
    );
    assert!(
        results.job_stats.p50 <= Duration::from_millis(30),
        "queue claim/ack P50 ({:?}) must be <= 30ms",
        results.job_stats.p50
    );
    assert!(
        results.job_stats.p95 <= Duration::from_millis(70),
        "queue claim/ack P95 ({:?}) must be <= 70ms",
        results.job_stats.p95
    );
    assert!(
        results.job_stats.p99 <= Duration::from_millis(150),
        "queue claim/ack P99 ({:?}) must be <= 150ms",
        results.job_stats.p99
    );

    // 5. Full JobWorker Runtime Acceptance Criteria & Latency Gates
    assert_eq!(
        results.worker_runtime_stats.errors, 0,
        "full JobWorker runtime error rate must be 0.0% (reconciled against database)"
    );
    assert!(
        results.worker_runtime_stats.throughput >= 30.0,
        "full JobWorker runtime throughput ({:.0} jobs/s) must exceed 30 jobs/s",
        results.worker_runtime_stats.throughput
    );
    assert!(
        results.worker_runtime_stats.p50 <= Duration::from_millis(250),
        "JobWorker runtime P50 ({:?}) must be <= 250ms",
        results.worker_runtime_stats.p50
    );
    assert!(
        results.worker_runtime_stats.p95 <= Duration::from_millis(350),
        "JobWorker runtime P95 ({:?}) must be <= 350ms",
        results.worker_runtime_stats.p95
    );
    assert!(
        results.worker_runtime_stats.p99 <= Duration::from_millis(450),
        "JobWorker runtime P99 ({:?}) must be <= 450ms",
        results.worker_runtime_stats.p99
    );
}
