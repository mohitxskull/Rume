//! Production Redis integration tests using Testcontainers.
//!
//! Validates:
//! - Real Redis connection pool lifecycle and health ping checks
//! - Key-value caching, existence, and deletion
//! - Live millisecond TTL expiration (`SET ... PX`)
//! - Set-based multi-key tag invalidation
//! - Typed [`Cache`] wrapper with cache-aside semantics

#![cfg(feature = "redis")]

use std::sync::Arc;
use std::time::Duration;

use rume::auth::{RedisSessionStore, Session, SessionStore};
use rume::cache::redis::RedisCacheBackend;
use rume::cache::{Cache, CacheBackend, CacheConfig, CacheError};
use rume::coordination::{
    DistributedLock, LeaderElector, RedisDistributedLock, RedisLeaderElector,
};
use rume::jobs::{
    HeartbeatRegistry, IdempotencyStatus, IdempotencyStore, RedisHeartbeatRegistry,
    RedisIdempotencyStore,
};
use rume::resilience::{RateLimitConfig, RedisRateLimiter, ResilienceError};
use rume::tenant::{RedisTenantQuotaStore, TenantScopedCache};
use serde::{Deserialize, Serialize};

use testcontainers_modules::redis::Redis;

mod common;

/// Helper to start a Redis alpine container and construct a connected backend.
async fn setup_redis_backend() -> (
    &'static testcontainers_modules::testcontainers::ContainerAsync<Redis>,
    RedisCacheBackend,
) {
    let node = common::containers::shared_redis().await;
    let backend = common::containers::create_isolated_redis_backend().await;
    (node, backend)
}

#[tokio::test]
async fn test_redis_pool_lifecycle_and_ping() {
    let (_container, backend) = setup_redis_backend().await;

    // Verify connectivity via PING command
    backend.ping().await.expect("ping failed");
}

#[tokio::test]
async fn test_redis_cache_get_set_and_delete() {
    let (_container, backend) = setup_redis_backend().await;

    // 1. Set key
    backend
        .set("session:abc123", b"authenticated-user".to_vec(), None)
        .await
        .expect("set failed");

    // 2. Verify exists
    let exists = backend
        .exists("session:abc123")
        .await
        .expect("exists failed");
    assert!(exists);

    // 3. Get key
    let value = backend.get("session:abc123").await.expect("get failed");
    assert_eq!(value.as_deref(), Some(b"authenticated-user".as_slice()));

    // 4. Delete key
    let deleted = backend
        .delete("session:abc123")
        .await
        .expect("delete failed");
    assert!(deleted);

    // 5. Verify gone
    let exists_after = backend
        .exists("session:abc123")
        .await
        .expect("exists failed");
    assert!(!exists_after);

    let value_after = backend.get("session:abc123").await.expect("get failed");
    assert_eq!(value_after, None);
}

#[tokio::test]
async fn test_redis_cache_ttl_expiration() {
    let (_container, backend) = setup_redis_backend().await;

    // Set with short 150ms TTL
    backend
        .set(
            "temp:token",
            b"ephemeral-data".to_vec(),
            Some(Duration::from_millis(150)),
        )
        .await
        .expect("set failed");

    // Immediately present
    let initial = backend.get("temp:token").await.expect("get failed");
    assert_eq!(initial.as_deref(), Some(b"ephemeral-data".as_slice()));

    // Wait for Redis TTL eviction
    tokio::time::sleep(Duration::from_millis(250)).await;

    // Value must be expired by the Redis server
    let expired = backend.get("temp:token").await.expect("get failed");
    assert_eq!(expired, None);
}

#[tokio::test]
async fn test_redis_cache_tag_invalidation() {
    let (_container, backend) = setup_redis_backend().await;

    // Associate posts with author and category tags
    backend
        .set_with_tags(
            "article:101",
            b"rust async programming".to_vec(),
            &["author:john", "topic:rust"],
            None,
        )
        .await
        .expect("set_with_tags failed");

    backend
        .set_with_tags(
            "article:102",
            b"distributed consensus in rust".to_vec(),
            &["author:john", "topic:distributed"],
            None,
        )
        .await
        .expect("set_with_tags failed");

    backend
        .set_with_tags(
            "article:103",
            b"intro to python".to_vec(),
            &["author:jane", "topic:python"],
            None,
        )
        .await
        .expect("set_with_tags failed");

    // Invalidate all articles by author "john"
    let invalidated = backend
        .invalidate_tag("author:john")
        .await
        .expect("invalidate_tag failed");
    assert_eq!(invalidated, 2);

    // John's articles are evicted
    assert_eq!(backend.get("article:101").await.unwrap(), None);
    assert_eq!(backend.get("article:102").await.unwrap(), None);

    // Jane's article remains intact
    assert_eq!(
        backend.get("article:103").await.unwrap().as_deref(),
        Some(b"intro to python".as_slice())
    );
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct UserProfile {
    id: String,
    username: String,
    tier: String,
}

#[tokio::test]
async fn test_redis_cache_typed_wrapper_cache_aside() {
    let (_container, backend) = setup_redis_backend().await;
    let cache = Cache::new(backend, rume::cache::CacheConfig::default());

    let fetch_counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));

    // First call: cache miss, executes loader closure
    let fc1 = Arc::clone(&fetch_counter);
    let profile1: UserProfile = cache
        .remember(
            "user_prof:42",
            Some(Duration::from_secs(60)),
            || async move {
                fc1.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(UserProfile {
                    id: "42".into(),
                    username: "alice_enterprise".into(),
                    tier: "premium".into(),
                })
            },
        )
        .await
        .expect("cache remember failed");

    assert_eq!(profile1.username, "alice_enterprise");
    assert_eq!(fetch_counter.load(std::sync::atomic::Ordering::SeqCst), 1);

    // Second call: cache hit from live Redis, loader closure is skipped!
    let fc2 = Arc::clone(&fetch_counter);
    let profile2: UserProfile = cache
        .remember(
            "user_prof:42",
            Some(Duration::from_secs(60)),
            || async move {
                fc2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(UserProfile {
                    id: "42".into(),
                    username: "alice_enterprise".into(),
                    tier: "premium".into(),
                })
            },
        )
        .await
        .expect("cache remember failed");

    assert_eq!(profile2, profile1);
    assert_eq!(
        fetch_counter.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "loader closure must not be executed on cache hit"
    );
}

#[tokio::test]
async fn test_redis_cluster_wide_tenant_quota_coordination() {
    let (_container, backend) = setup_redis_backend().await;
    let pool = backend.pool().clone();

    // Two independent app server instances sharing the same distributed Redis quota store
    let shared_quota_store: Arc<dyn rume::tenant::TenantQuotaStore> =
        Arc::new(RedisTenantQuotaStore::new(pool.clone()));

    let server_a = TenantScopedCache::new(Cache::new(backend.clone(), CacheConfig::default()))
        .with_max_keys_per_tenant(3)
        .with_quota_store(Arc::clone(&shared_quota_store));

    let server_b = TenantScopedCache::new(Cache::new(backend.clone(), CacheConfig::default()))
        .with_max_keys_per_tenant(3)
        .with_quota_store(Arc::clone(&shared_quota_store));

    // Server A caches 2 keys for tenant "tenant-corp"
    server_a
        .set("tenant-corp", "doc:1", &"data 1", None)
        .await
        .expect("server A set doc:1 failed");
    server_a
        .set("tenant-corp", "doc:2", &"data 2", None)
        .await
        .expect("server A set doc:2 failed");

    assert_eq!(server_a.active_key_count("tenant-corp").await.unwrap(), 2);

    // Server B caches 1 key for tenant "tenant-corp" (now total 3, hitting quota limit of 3)
    server_b
        .set("tenant-corp", "doc:3", &"data 3", None)
        .await
        .expect("server B set doc:3 failed");

    assert_eq!(server_b.active_key_count("tenant-corp").await.unwrap(), 3);

    // Now Server A attempts to cache a 4th key -> REJECTED cluster-wide by Redis quota store!
    let attempt_overflow = server_a
        .set("tenant-corp", "doc:4", &"overflow", None)
        .await;
    match attempt_overflow {
        Err(CacheError::QuotaExceeded(msg)) => {
            assert!(msg.contains("tenant 'tenant-corp' exceeded quota of 3 keys"));
        }
        other => panic!("expected QuotaExceeded error, got: {other:?}"),
    }

    // Server B deletes one key
    server_b
        .delete("tenant-corp", "doc:3")
        .await
        .expect("delete failed");
    assert_eq!(server_a.active_key_count("tenant-corp").await.unwrap(), 2);

    // Server A can now store doc:4
    server_a
        .set("tenant-corp", "doc:4", &"data 4", None)
        .await
        .expect("server A should now store doc:4");

    // Invalidate full tenant -> resets quota count to 0 in Redis
    server_b
        .invalidate_tenant("tenant-corp")
        .await
        .expect("invalidate_tenant failed");
    assert_eq!(server_a.active_key_count("tenant-corp").await.unwrap(), 0);
}

#[tokio::test]
async fn test_redis_sliding_window_rate_limiter() {
    let (_container, backend) = setup_redis_backend().await;
    let pool = backend.pool().clone();

    let config = RateLimitConfig {
        max_requests: 3,
        window: Duration::from_millis(500),
    };

    let limiter = RedisRateLimiter::new(pool, config);

    // 1st request -> remaining = 2
    let s1 = limiter.check("client-10.0.0.1").await.expect("check 1");
    assert_eq!(s1.remaining, 2);

    // 2nd request -> remaining = 1
    let s2 = limiter.check("client-10.0.0.1").await.expect("check 2");
    assert_eq!(s2.remaining, 1);

    // 3rd request -> remaining = 0
    let s3 = limiter.check("client-10.0.0.1").await.expect("check 3");
    assert_eq!(s3.remaining, 0);

    // 4th request -> RateLimited
    let err = limiter.check("client-10.0.0.1").await;
    match err {
        Err(ResilienceError::RateLimited { retry_after_secs }) => {
            assert!(retry_after_secs >= 1);
        }
        other => panic!("expected RateLimited error, got: {other:?}"),
    }

    // A different client is NOT affected
    let other_client = limiter
        .check("client-10.0.0.2")
        .await
        .expect("other client");
    assert_eq!(other_client.remaining, 2);

    // Wait for sliding window to elapse (550ms)
    tokio::time::sleep(Duration::from_millis(550)).await;

    // First client can make requests again!
    let s_after = limiter
        .check("client-10.0.0.1")
        .await
        .expect("check after window");
    assert_eq!(s_after.remaining, 2);
}

#[tokio::test]
async fn test_redis_distributed_lock_mutual_exclusion_and_fencing() {
    let (_container, backend) = setup_redis_backend().await;
    let pool = backend.pool().clone();

    let node_a = RedisDistributedLock::new(pool.clone());
    let node_b = RedisDistributedLock::new(pool.clone());

    // 1. Node A acquires lock on "report-generator"
    let guard_a = node_a
        .acquire("report-generator", Duration::from_secs(5))
        .await
        .expect("acquire node_a")
        .expect("should acquire guard");

    assert_eq!(guard_a.resource, "report-generator");
    assert_eq!(guard_a.fencing_token, 1);

    // 2. Node B attempts acquisition on same resource -> returns None
    let guard_b = node_b
        .acquire("report-generator", Duration::from_secs(5))
        .await
        .expect("acquire node_b");
    assert!(guard_b.is_none());

    // 3. Node A renews active lock
    node_a
        .renew(&guard_a, Duration::from_secs(5))
        .await
        .expect("renewal should succeed");

    // 4. Node A releases lock
    let released = node_a.release(guard_a).await.expect("release");
    assert!(released);

    // 5. Node B can now acquire, receiving monotonically increased fencing token
    let guard_b_new = node_b
        .acquire("report-generator", Duration::from_secs(5))
        .await
        .expect("acquire node_b after release")
        .expect("guard present");

    assert_eq!(guard_b_new.fencing_token, 2);
    node_b.release(guard_b_new).await.expect("release b");
}

#[tokio::test]
async fn test_redis_leader_election_failover_and_terms() {
    let (_container, backend) = setup_redis_backend().await;
    let pool = backend.pool().clone();

    let elector_a = RedisLeaderElector::new(pool.clone());
    let elector_b = RedisLeaderElector::new(pool.clone());

    // 1. Elector A acquires leadership for "scheduler"
    let lease_a = elector_a
        .try_acquire("scheduler", "worker-1", Duration::from_secs(5))
        .await
        .expect("acquire a")
        .expect("lease present");

    assert_eq!(lease_a.role, "scheduler");
    assert_eq!(lease_a.candidate_id, "worker-1");
    assert_eq!(lease_a.term, 1);

    // 2. Elector B fails to acquire active leadership
    let lease_b = elector_b
        .try_acquire("scheduler", "worker-2", Duration::from_secs(5))
        .await
        .expect("acquire b");
    assert!(lease_b.is_none());

    // 3. Query active leader returns Elector A
    let leader = elector_b
        .get_leader("scheduler")
        .await
        .expect("get leader")
        .expect("leader present");
    assert_eq!(leader.candidate_id, "worker-1");
    assert_eq!(leader.term, 1);

    // 4. Elector A renews
    let renewed = elector_a
        .renew("scheduler", "worker-1", Duration::from_secs(5))
        .await
        .expect("renew")
        .expect("renewed lease");
    assert_eq!(renewed.term, 1);

    // 5. Elector A resigns
    let resigned = elector_a
        .resign("scheduler", "worker-1")
        .await
        .expect("resign");
    assert!(resigned);

    // 6. Elector B acquires with next term
    let lease_b_new = elector_b
        .try_acquire("scheduler", "worker-2", Duration::from_secs(5))
        .await
        .expect("acquire b new")
        .expect("lease b present");

    assert_eq!(lease_b_new.candidate_id, "worker-2");
    assert_eq!(lease_b_new.term, 2);
}

#[tokio::test]
async fn test_redis_heartbeat_registry_and_dead_worker_reaping() {
    let (_container, backend) = setup_redis_backend().await;
    let pool = backend.pool().clone();

    let registry = RedisHeartbeatRegistry::new(pool);

    // Record worker-1 with short TTL (100ms) and worker-2 with long TTL (10s)
    registry
        .record_heartbeat("worker-1", Duration::from_millis(100))
        .await
        .expect("record 1");
    registry
        .record_heartbeat("worker-2", Duration::from_secs(10))
        .await
        .expect("record 2");

    assert!(registry.is_alive("worker-1").await.expect("is alive 1"));
    assert!(registry.is_alive("worker-2").await.expect("is alive 2"));

    // Wait for worker-1 to expire
    tokio::time::sleep(Duration::from_millis(150)).await;

    assert!(!registry.is_alive("worker-1").await.expect("is alive 1"));
    assert!(registry.is_alive("worker-2").await.expect("is alive 2"));

    // Reap dead workers
    let reaped = registry.reap_dead_workers().await.expect("reap");
    assert_eq!(reaped, vec!["worker-1"]);

    // Verify worker-2 remains active
    assert!(registry.is_alive("worker-2").await.expect("is alive 2"));
}

#[tokio::test]
async fn test_redis_idempotency_store() {
    let (_container, backend) = setup_redis_backend().await;
    let pool = backend.pool().clone();

    let store = RedisIdempotencyStore::new(pool);

    // 1. Initial attempt -> New
    let s1 = store
        .try_acquire("tx-uuid-999", Duration::from_secs(10))
        .await
        .expect("acquire");
    assert_eq!(s1, IdempotencyStatus::New);

    // 2. Concurrent attempt while in progress -> InProgress
    let s2 = store
        .try_acquire("tx-uuid-999", Duration::from_secs(10))
        .await
        .expect("acquire 2");
    assert_eq!(s2, IdempotencyStatus::InProgress);

    // 3. Mark completed with payload
    store
        .complete("tx-uuid-999", Some(b"order-id:456".to_vec()))
        .await
        .expect("complete");

    // 4. Subsequent attempt -> Completed with cached payload
    let s3 = store
        .try_acquire("tx-uuid-999", Duration::from_secs(10))
        .await
        .expect("acquire 3");
    assert_eq!(
        s3,
        IdempotencyStatus::Completed(Some(b"order-id:456".to_vec()))
    );

    // 5. Release key -> allows new execution
    store.release("tx-uuid-999").await.expect("release");
    let s4 = store
        .try_acquire("tx-uuid-999", Duration::from_secs(10))
        .await
        .expect("acquire 4");
    assert_eq!(s4, IdempotencyStatus::New);
}

#[tokio::test]
async fn test_redis_session_store_save_load_ttl_and_destroy() {
    let (_container, backend) = setup_redis_backend().await;
    let pool = backend.pool().clone();

    let server_1 = RedisSessionStore::new(pool.clone());
    let server_2 = RedisSessionStore::new(pool.clone());

    let mut session = Session::new("session_xyz", "user_admin", 10)
        .with_role("admin")
        .with_permission("users:write");
    session
        .data
        .insert("ip".to_string(), "192.168.1.1".to_string());

    // 1. Server 1 saves session
    server_1.save(session.clone()).await.expect("save session");

    // 2. Server 2 loads session (multi-instance session sharing)
    let loaded = server_2
        .load("session_xyz")
        .await
        .expect("load session")
        .expect("session present");

    assert_eq!(loaded.id, "session_xyz");
    assert_eq!(loaded.user_id, "user_admin");
    assert_eq!(loaded.roles, vec!["admin"]);
    assert_eq!(loaded.permissions, vec!["users:write"]);
    assert_eq!(loaded.data.get("ip").unwrap(), "192.168.1.1");

    // 3. Server 2 destroys session
    server_2
        .destroy("session_xyz")
        .await
        .expect("destroy session");
    assert!(server_1.load("session_xyz").await.unwrap().is_none());

    // 4. Session TTL expiration: 1s TTL
    let short_session = Session::new("session_temp", "user_guest", 1);
    server_1.save(short_session).await.expect("save temp");
    assert!(server_2.load("session_temp").await.unwrap().is_some());

    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert!(server_2.load("session_temp").await.unwrap().is_none());
}
