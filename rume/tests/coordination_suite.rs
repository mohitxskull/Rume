use rume::prelude::*;
use std::time::Duration;

#[tokio::test]
async fn test_distributed_lock_exclusive_and_fencing() {
    let lock_service = MemoryDistributedLock::new();
    let ttl = Duration::from_millis(50);

    // 1. Client A acquires lock
    let guard_a = lock_service.acquire("db-export", ttl).await.unwrap();
    assert!(guard_a.is_some());
    let guard_a = guard_a.unwrap();
    assert_eq!(guard_a.resource, "db-export");
    assert_eq!(guard_a.fencing_token, 1);

    // 2. Client B tries to acquire same resource while active - must fail
    let guard_b = lock_service.acquire("db-export", ttl).await.unwrap();
    assert!(guard_b.is_none());

    // 3. Client A renews lock
    lock_service
        .renew(&guard_a, Duration::from_millis(100))
        .await
        .unwrap();

    // 4. Client A releases lock
    let released = lock_service.release(guard_a).await.unwrap();
    assert!(released);

    // 5. Client B now succeeds in acquiring the lock, getting monotonic fencing token 2
    let guard_b = lock_service.acquire("db-export", ttl).await.unwrap();
    assert!(guard_b.is_some());
    let guard_b = guard_b.unwrap();
    assert_eq!(guard_b.fencing_token, 2);
}

#[tokio::test]
async fn test_distributed_lock_ttl_auto_expiration() {
    let lock_service = MemoryDistributedLock::new();
    let ttl = Duration::from_millis(20);

    // Client A acquires short lock
    let guard_a = lock_service
        .acquire("cache-rebuild", ttl)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(guard_a.fencing_token, 1);

    // Sleep past TTL
    tokio::time::sleep(Duration::from_millis(30)).await;

    // Client B acquires expired lock
    let guard_b = lock_service.acquire("cache-rebuild", ttl).await.unwrap();
    assert!(guard_b.is_some());
    assert_eq!(guard_b.unwrap().fencing_token, 2);

    // Client A attempts to renew expired lock - must fail with LockExpired
    let renew_result = lock_service.renew(&guard_a, ttl).await;
    assert!(matches!(
        renew_result,
        Err(CoordinationError::LockUnavailable { .. } | CoordinationError::LockExpired { .. })
    ));
}

#[tokio::test]
async fn test_leader_election_lifecycle() {
    let elector = MemoryLeaderElector::new();
    let role = "scheduler-primary";
    let ttl = Duration::from_millis(50);

    // 1. Candidate 1 acquires leadership with initial term 1
    let lease_a = elector
        .try_acquire(role, "worker-node-1", ttl)
        .await
        .unwrap();
    assert!(lease_a.is_some());
    let lease_a = lease_a.unwrap();
    assert_eq!(lease_a.role, role);
    assert_eq!(lease_a.candidate_id, "worker-node-1");
    assert_eq!(lease_a.term, 1);

    assert_eq!(
        elector.get_leader(role).await.unwrap(),
        Some(LeaderLease {
            role: role.to_string(),
            candidate_id: "worker-node-1".to_string(),
            term: 1,
        })
    );

    // 2. Candidate 2 attempts to acquire - rejected while active
    let rejected = elector
        .try_acquire(role, "worker-node-2", ttl)
        .await
        .unwrap();
    assert!(rejected.is_none());

    // 3. Candidate 1 renews lease - term remains 1
    let renewed = elector.renew(role, "worker-node-1", ttl).await.unwrap();
    assert!(renewed.is_some());
    assert_eq!(renewed.unwrap().term, 1);

    // 4. Candidate 1 resigns leadership
    let resigned = elector.resign(role, "worker-node-1").await.unwrap();
    assert!(resigned);
    assert_eq!(elector.get_leader(role).await.unwrap(), None);

    // 5. Candidate 2 now becomes leader, receiving next monotonic term 2
    let lease_b = elector
        .try_acquire(role, "worker-node-2", ttl)
        .await
        .unwrap();
    assert!(lease_b.is_some());
    let lease_b = lease_b.unwrap();
    assert_eq!(lease_b.candidate_id, "worker-node-2");
    assert_eq!(lease_b.term, 2);
    assert_eq!(
        elector.get_leader(role).await.unwrap(),
        Some(LeaderLease {
            role: role.to_string(),
            candidate_id: "worker-node-2".to_string(),
            term: 2,
        })
    );
}
