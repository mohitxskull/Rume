//! Concurrency interleaving invariants and atomic state transition verification.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::time::Duration;

use rume::app::state::{Availability, AvailabilityState, Lifecycle, LifecycleState};
use rume::cache::{Cache, CacheConfig, InMemoryCacheBackend};
use rume::persistence::config::DatabaseConfig;
use rume::persistence::error::PersistenceError;
use rume::persistence::pool::DatabasePool;
use rume::persistence::sqlite::SqliteBackend;

#[tokio::test]
async fn pool_lease_capacity_and_exhaustion_invariants_under_load() {
    let backend = SqliteBackend::in_memory()
        .await
        .expect("in-memory sqlite connects");
    let config = DatabaseConfig::default()
        .with_max_connections(5)
        .with_connect_timeout(2);

    let pool = DatabasePool::new(backend, config);
    let peak_active = Arc::new(AtomicUsize::new(0));
    let current_active = Arc::new(AtomicUsize::new(0));

    let mut handles = Vec::new();
    for _ in 0..50 {
        let pool = pool.clone();
        let peak = Arc::clone(&peak_active);
        let curr = Arc::clone(&current_active);

        handles.push(tokio::spawn(async move {
            // Attempt to acquire lease with timeout
            let lease = pool.acquire_lease_timeout(Duration::from_millis(500)).await;
            if let Ok(_lease) = lease {
                let count = curr.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(count, Ordering::SeqCst);
                assert!(
                    count <= 5,
                    "invariant violated: active leases ({count}) exceeded max_connections (5)"
                );

                tokio::time::sleep(Duration::from_millis(10)).await;
                curr.fetch_sub(1, Ordering::SeqCst);
            }
        }));
    }

    for handle in handles {
        handle.await.expect("task completes");
    }

    let status = pool.status();
    assert_eq!(status.leased_connections, 0, "all leases must be released");
    assert!(
        peak_active.load(Ordering::SeqCst) <= 5,
        "peak leased must never exceed 5"
    );
    assert!(
        peak_active.load(Ordering::SeqCst) > 0,
        "at least some leases must have succeeded"
    );
}

#[tokio::test]
async fn pool_close_and_drain_interleaving_with_active_workers() {
    let backend = SqliteBackend::in_memory()
        .await
        .expect("in-memory sqlite connects");
    let config = DatabaseConfig::default().with_max_connections(10);
    let pool = DatabasePool::new(backend, config);

    let started_barrier = Arc::new(tokio::sync::Barrier::new(4));
    let mut worker_handles = Vec::new();

    // Spawn 3 workers holding leases for 50ms
    for _ in 0..3 {
        let pool = pool.clone();
        let barrier = Arc::clone(&started_barrier);
        worker_handles.push(tokio::spawn(async move {
            let _lease = pool.acquire_lease().expect("lease succeeds before close");
            barrier.wait().await;
            tokio::time::sleep(Duration::from_millis(50)).await;
        }));
    }

    // Wait until workers have acquired their leases
    started_barrier.wait().await;
    assert_eq!(pool.status().leased_connections, 3);

    // Concurrently trigger close_and_drain
    let pool_clone = pool.clone();
    let drain_handle =
        tokio::spawn(async move { pool_clone.close_and_drain(Duration::from_millis(500)).await });

    // Wait until drain has initiated (is_closed is atomic)
    while !pool.status().is_closed {
        tokio::task::yield_now().await;
    }

    // Concurrently verify that new lease acquisitions are rejected immediately
    for _ in 0..5 {
        let err = pool.acquire_lease();
        assert!(
            matches!(err, Err(PersistenceError::Closed)),
            "new lease requests must fail with Closed during drain"
        );
    }

    for worker in worker_handles {
        worker.await.expect("worker completed");
    }

    let drained = drain_handle.await.expect("drain task finished");
    assert!(drained, "pool must cleanly drain active leases");
    assert_eq!(pool.status().leased_connections, 0);
    assert!(pool.status().is_closed);
}

#[tokio::test]
async fn singleflight_stampede_protection_interleaving_executes_closure_once() {
    let backend = InMemoryCacheBackend::new();
    let config = CacheConfig::new("memory://test").with_prefix("test-prefix:");
    let cache = Cache::new(backend, config);
    let computation_runs = Arc::new(AtomicU32::new(0));

    let mut tasks = Vec::new();
    for _ in 0..50 {
        let cache = cache.clone();
        let runs = Arc::clone(&computation_runs);

        tasks.push(tokio::spawn(async move {
            cache
                .remember_with_tags(
                    "stampede_key",
                    &["tag1"],
                    Some(Duration::from_secs(60)),
                    || async {
                        runs.fetch_add(1, Ordering::SeqCst);
                        tokio::time::sleep(Duration::from_millis(20)).await;
                        Ok::<_, rume::cache::CacheError>("computed_value".to_string())
                    },
                )
                .await
        }));
    }

    for task in tasks {
        let result = task
            .await
            .expect("task finishes")
            .expect("remember_json succeeds");
        assert_eq!(result, "computed_value");
    }

    assert_eq!(
        computation_runs.load(Ordering::SeqCst),
        1,
        "singleflight stampede protection must execute the factory closure exactly once"
    );
}

#[test]
fn lifecycle_monotonic_state_transition_under_concurrent_readers() {
    let lifecycle = Lifecycle::new();
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));

    let mut readers = Vec::new();
    for _ in 0..8 {
        let lc = lifecycle.clone();
        let s = Arc::clone(&stop);
        readers.push(std::thread::spawn(move || {
            let mut last_state = LifecycleState::Created as u8;
            while !s.load(Ordering::Relaxed) {
                let curr = lc.state() as u8;
                assert!(
                    curr >= last_state,
                    "lifecycle state observed retrograde transition from {last_state} to {curr}"
                );
                last_state = curr;
            }
        }));
    }

    // Step through the states monotonically
    std::thread::sleep(Duration::from_millis(5));
    lifecycle.set(LifecycleState::Booted);
    std::thread::sleep(Duration::from_millis(5));
    lifecycle.set(LifecycleState::Ready);
    std::thread::sleep(Duration::from_millis(5));
    lifecycle.set(LifecycleState::Terminating);
    std::thread::sleep(Duration::from_millis(5));
    lifecycle.set(LifecycleState::Terminated);
    std::thread::sleep(Duration::from_millis(5));

    stop.store(true, Ordering::Relaxed);
    for reader in readers {
        reader.join().expect("reader thread completed successfully");
    }

    assert!(lifecycle.is_terminated());
}

#[test]
fn availability_state_concurrency_stress() {
    let availability = Availability::new();
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));

    let mut handles = Vec::new();
    for _ in 0..8 {
        let av = availability.clone();
        let s = Arc::clone(&stop);
        handles.push(std::thread::spawn(move || {
            while !s.load(Ordering::Relaxed) {
                let state = av.state();
                assert!(
                    matches!(
                        state,
                        AvailabilityState::Unknown
                            | AvailabilityState::Live
                            | AvailabilityState::Ready
                            | AvailabilityState::RefusingTraffic
                            | AvailabilityState::Broken
                    ),
                    "availability state must always be a valid discriminant"
                );
            }
        }));
    }

    // Interleave updates
    for _ in 0..100 {
        availability.set(AvailabilityState::Live);
        availability.set(AvailabilityState::Ready);
        availability.set(AvailabilityState::RefusingTraffic);
        availability.set(AvailabilityState::Ready);
    }

    stop.store(true, Ordering::Relaxed);
    for h in handles {
        h.join().expect("thread finishes");
    }
}
