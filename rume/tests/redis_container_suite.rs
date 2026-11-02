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

use rume::cache::redis::RedisCacheBackend;
use rume::cache::{Cache, CacheBackend};
use serde::{Deserialize, Serialize};

use testcontainers_modules::redis::{REDIS_PORT, Redis};
use testcontainers_modules::testcontainers::ImageExt;
use testcontainers_modules::testcontainers::runners::AsyncRunner;

/// Helper to start a Redis alpine container and construct a connected backend.
async fn setup_redis_backend() -> (
    testcontainers_modules::testcontainers::ContainerAsync<Redis>,
    RedisCacheBackend,
) {
    let container = Redis::default()
        .with_tag("alpine")
        .start()
        .await
        .expect("Failed to start Redis container");

    let host = container
        .get_host()
        .await
        .expect("Failed to get container host");
    let port = container
        .get_host_port_ipv4(REDIS_PORT)
        .await
        .expect("Failed to get mapped port");

    let url = format!("redis://{host}:{port}");
    let backend = RedisCacheBackend::new(&url).expect("Failed to initialize RedisCacheBackend");

    (container, backend)
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
