#![cfg(feature = "redis")]

use std::time::Duration;
use rume::coordination::*;
use crate::common;

async fn start_redis_coordination() -> (
    &'static testcontainers_modules::testcontainers::ContainerAsync<testcontainers_modules::redis::Redis>,
    RedisDistributedLock,
    RedisLeaderElector,
) {
    let node = common::containers::shared_redis().await;
    let pool = common::containers::create_isolated_redis_pool().await;
    let lock = RedisDistributedLock::new(pool.clone());
    let elector = RedisLeaderElector::new(pool);
    (node, lock, elector)
}

#[tokio::test]
async fn test_distributed_lock_exclusive_and_fencing() {
    let (_node, lock_service, _elector) = start_redis_coordination().await;
    let ttl = Duration::from_millis(1500);

    // 1. Client A acquires lock
    let guard_a = lock_service.acquire("db-export", ttl).await.unwrap();
    assert!(guard_a.is_some());
    let guard_a = guard_a.unwrap();
    assert_eq!(guard_a.resource, "db-export");
    assert_eq!(guard_a.fencing_token, 1);

    // 2. Client B cannot acquire while A holds it
    let guard_b = lock_service.acquire("db-export", ttl).await.unwrap();
    assert!(guard_b.is_none());

    // 3. Client A renews lock
    lock_service.renew(&guard_a, ttl).await.unwrap();

    // 4. Client A releases lock
    let released = lock_service.release(guard_a).await.unwrap();
    assert!(released);

    // 5. Client B acquires lock successfully with monotonically increasing fencing token
    let guard_b = lock_service.acquire("db-export", ttl).await.unwrap();
    assert!(guard_b.is_some());
    let guard_b = guard_b.unwrap();
    assert_eq!(guard_b.fencing_token, 2);
}

#[tokio::test]
async fn test_distributed_lock_ttl_auto_expiration() {
    let (_node, lock_service, _elector) = start_redis_coordination().await;
    let short_ttl = Duration::from_millis(50);

    // Client A acquires lock with very short TTL
    let guard_a = lock_service.acquire("batch-job", short_ttl).await.unwrap();
    assert!(guard_a.is_some());

    // Poll until A's lease expires automatically in Redis
    let mut guard_b = None;
    for _ in 0..25 {
        if let Ok(Some(g)) = lock_service.acquire("batch-job", short_ttl).await {
            guard_b = Some(g);
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(guard_b.is_some(), "Client B should acquire once A's lease expires");
}

#[tokio::test]
async fn test_leader_election_lifecycle() {
    let (_node, _lock_service, elector) = start_redis_coordination().await;
    let role = "scheduler-leader";
    let ttl = Duration::from_millis(200);

    // 1. Node 1 acquires leadership
    let lease1 = elector.try_acquire(role, "node-1", ttl).await.unwrap();
    assert!(lease1.is_some());
    let lease1 = lease1.unwrap();
    assert_eq!(lease1.role, role);
    assert_eq!(lease1.candidate_id, "node-1");
    assert_eq!(lease1.term, 1);

    // 2. Node 2 fails to acquire while Node 1 is leader
    let lease2 = elector.try_acquire(role, "node-2", ttl).await.unwrap();
    assert!(lease2.is_none());

    // 3. Node 1 renews leadership lease
    let renewed = elector.renew(role, "node-1", ttl).await.unwrap();
    assert!(renewed.is_some());
    assert_eq!(renewed.unwrap().term, 1);

    // 4. Node 1 steps down
    let resigned = elector.resign(role, "node-1").await.unwrap();
    assert!(resigned);

    // 5. Node 2 can now acquire leadership with incremented term
    let lease2 = elector.try_acquire(role, "node-2", ttl).await.unwrap();
    assert!(lease2.is_some());
    let lease2 = lease2.unwrap();
    assert_eq!(lease2.candidate_id, "node-2");
    assert_eq!(lease2.term, 2);

    // 6. Leadership auto-fails over on crash (TTL expiry)
    let mut lease3 = None;
    for _ in 0..30 {
        if let Ok(Some(l)) = elector.try_acquire(role, "node-3", ttl).await {
            lease3 = Some(l);
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(lease3.is_some(), "Node 3 should acquire leadership after TTL expiry");
    let lease3 = lease3.unwrap();
    assert_eq!(lease3.candidate_id, "node-3");
    assert_eq!(lease3.term, 3);
}
