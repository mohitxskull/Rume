use std::time::Duration;

use rume::jobs::{
    HeartbeatRegistry, IdempotencyStatus, IdempotencyStore, MemoryHeartbeatRegistry,
    MemoryIdempotencyStore,
};
use tokio::time::sleep;

#[tokio::test]
async fn idempotency_store_lifecycle() {
    let store = MemoryIdempotencyStore::new();

    // 1. Initial attempt: New
    let status = store
        .try_acquire("req_order_123", Duration::from_secs(10))
        .await
        .unwrap();
    assert_eq!(status, IdempotencyStatus::New);

    // 2. Second attempt while in progress: InProgress
    let status2 = store
        .try_acquire("req_order_123", Duration::from_secs(10))
        .await
        .unwrap();
    assert_eq!(status2, IdempotencyStatus::InProgress);

    // 3. Complete execution
    store
        .complete("req_order_123", Some(b"order_created".to_vec()))
        .await
        .unwrap();

    // 4. Third attempt after completion: Completed with saved result
    let status3 = store
        .try_acquire("req_order_123", Duration::from_secs(10))
        .await
        .unwrap();
    assert_eq!(
        status3,
        IdempotencyStatus::Completed(Some(b"order_created".to_vec()))
    );

    // 5. Another key can be released
    let status_other = store
        .try_acquire("req_fail_456", Duration::from_secs(10))
        .await
        .unwrap();
    assert_eq!(status_other, IdempotencyStatus::New);

    store.release("req_fail_456").await.unwrap();

    let status_retry = store
        .try_acquire("req_fail_456", Duration::from_secs(10))
        .await
        .unwrap();
    assert_eq!(status_retry, IdempotencyStatus::New);
}

#[tokio::test]
async fn worker_heartbeat_tracking_and_reaping() {
    let tracker = MemoryHeartbeatRegistry::new();

    // Worker 1 alive with 20ms TTL
    tracker
        .record_heartbeat("worker_1", Duration::from_millis(20))
        .await
        .unwrap();

    // Worker 2 alive with 2000ms TTL
    tracker
        .record_heartbeat("worker_2", Duration::from_secs(2))
        .await
        .unwrap();

    assert!(tracker.is_alive("worker_1").await.unwrap());
    assert!(tracker.is_alive("worker_2").await.unwrap());

    // Sleep past Worker 1 TTL
    sleep(Duration::from_millis(30)).await;

    assert!(!tracker.is_alive("worker_1").await.unwrap());
    assert!(tracker.is_alive("worker_2").await.unwrap());

    // Reap dead workers
    let dead = tracker.reap_dead_workers().await.unwrap();
    assert_eq!(dead, vec!["worker_1"]);

    // Worker 2 is still active
    assert!(tracker.is_alive("worker_2").await.unwrap());
}
