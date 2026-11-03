#![cfg(feature = "redis")]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use testcontainers_modules::redis::Redis;
use testcontainers_modules::testcontainers::runners::AsyncRunner;

use rume::cache::{Cache, CacheConfig, RedisCacheBackend};
use rume::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct Product {
    id: String,
    title: String,
    price_cents: u64,
}

async fn start_redis_cache() -> (
    testcontainers_modules::testcontainers::ContainerAsync<Redis>,
    RedisCacheBackend,
    String,
) {
    let node = Redis::default().start().await.expect("start redis node");
    let port = node
        .get_host_port_ipv4(6379)
        .await
        .expect("get redis port");
    let url = format!("redis://127.0.0.1:{port}");
    let backend = RedisCacheBackend::new(&url).expect("create redis cache backend");
    (node, backend, url)
}

#[tokio::test]
async fn cache_crud_and_prefix_namespaces() {
    let (_node, backend, url) = start_redis_cache().await;
    let config = CacheConfig::new(&url)
        .with_default_ttl(60)
        .with_prefix("app:v1:");

    let cache = Cache::new(backend, config);

    let product = Product {
        id: "prod_100".into(),
        title: "Rust in Action".into(),
        price_cents: 4999,
    };

    // Initially does not exist
    assert!(!cache.exists("prod_100").await.unwrap());
    assert_eq!(cache.get_json::<Product>("prod_100").await.unwrap(), None);

    // Set product
    cache.set_json("prod_100", &product, None).await.unwrap();

    // Now exists
    assert!(cache.exists("prod_100").await.unwrap());
    let retrieved: Product = cache
        .get_json("prod_100")
        .await
        .unwrap()
        .expect("should be found");
    assert_eq!(retrieved, product);

    // Delete product
    assert!(cache.delete("prod_100").await.unwrap());
    assert!(!cache.exists("prod_100").await.unwrap());
}

#[tokio::test]
async fn cache_tags_invalidation() {
    let (_node, backend, url) = start_redis_cache().await;
    let config = CacheConfig::new(&url).with_default_ttl(60);
    let cache = Cache::new(backend, config);

    let p1 = Product {
        id: "p1".into(),
        title: "Book 1".into(),
        price_cents: 1000,
    };
    let p2 = Product {
        id: "p2".into(),
        title: "Book 2".into(),
        price_cents: 2000,
    };
    let p3 = Product {
        id: "p3".into(),
        title: "Audiobook".into(),
        price_cents: 3000,
    };

    // Store items with tags
    cache
        .set_json_with_tags("p1", &p1, &["books", "media"], None)
        .await
        .unwrap();
    cache
        .set_json_with_tags("p2", &p2, &["books"], None)
        .await
        .unwrap();
    cache
        .set_json_with_tags("p3", &p3, &["media"], None)
        .await
        .unwrap();

    assert!(cache.exists("p1").await.unwrap());
    assert!(cache.exists("p2").await.unwrap());
    assert!(cache.exists("p3").await.unwrap());

    // Invalidate books tag -> p1 and p2 are deleted; p3 remains
    cache.invalidate_tag("books").await.unwrap();

    assert!(!cache.exists("p1").await.unwrap());
    assert!(!cache.exists("p2").await.unwrap());
    assert!(cache.exists("p3").await.unwrap());

    // Invalidate media tag -> p3 deleted
    cache.invalidate_tag("media").await.unwrap();
    assert!(!cache.exists("p3").await.unwrap());
}

#[tokio::test]
async fn cache_typed_wrapper_convenience() {
    let (_node, backend, url) = start_redis_cache().await;
    let config = CacheConfig::new(&url).with_default_ttl(60);
    let cache = Cache::new(backend, config);

    let compute_counter = Arc::new(AtomicUsize::new(0));

    // Cache aside closure
    let compute = || {
        let count = Arc::clone(&compute_counter);
        async move {
            count.fetch_add(1, Ordering::SeqCst);
            Ok(Product {
                id: "computed_1".into(),
                title: "Expensive Computation".into(),
                price_cents: 1234,
            })
        }
    };

    // First call computes
    let p1: Product = cache.remember("expensive_item", None, compute).await.unwrap();
    assert_eq!(p1.id, "computed_1");
    assert_eq!(compute_counter.load(Ordering::SeqCst), 1);

    // Second call reads from cache without recomputing
    let p2: Product = cache
        .remember("expensive_item", None, compute)
        .await
        .unwrap();

    assert_eq!(p2.id, "computed_1");
    assert_eq!(compute_counter.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn cache_expiration_and_refresh() {
    let (_node, backend, url) = start_redis_cache().await;
    let config = CacheConfig::new(&url).with_default_ttl(60);
    let cache = Cache::new(backend, config);

    // Set with short TTL
    cache
        .set_json("short_lived", &42, Some(Duration::from_millis(150)))
        .await
        .unwrap();

    assert!(cache.exists("short_lived").await.unwrap());

    // Wait for TTL expiration in Redis
    tokio::time::sleep(Duration::from_millis(300)).await;

    assert!(!cache.exists("short_lived").await.unwrap());
    assert_eq!(cache.get_json::<i32>("short_lived").await.unwrap(), None);
}

#[tokio::test]
async fn cache_health_check_reports_diagnostics() {
    let (_node, backend, url) = start_redis_cache().await;
    let cache = Cache::new(backend, CacheConfig::new(&url));
    let hc = cache.health_check("app_cache");

    // Initially healthy
    let res = hc.run().await;
    assert_eq!(res.status, HealthStatus::Ok);
    assert_eq!(res.name, "app_cache");
    assert_eq!(
        res.meta.get("is_closed").unwrap(),
        &serde_json::json!(false)
    );

    // Closed cache
    cache.close();
    let res_closed = hc.run().await;
    assert_eq!(res_closed.status, HealthStatus::Failed);
    assert_eq!(res_closed.message, "cache is closed");
}

#[tokio::test]
async fn cache_remember_stampede_singleflight_protection() {
    let (_node, backend, url) = start_redis_cache().await;
    let cache = Arc::new(Cache::new(backend, CacheConfig::new(&url)));
    let fetch_count = Arc::new(AtomicUsize::new(0));

    let mut handles = Vec::new();
    for _ in 0..5 {
        let cache_clone = Arc::clone(&cache);
        let count_clone = Arc::clone(&fetch_count);
        handles.push(tokio::spawn(async move {
            cache_clone
                .remember("stampede_key", None, || async move {
                    count_clone.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(30)).await;
                    Ok(Product {
                        id: "shared".into(),
                        title: "Singleflight Product".into(),
                        price_cents: 42,
                    })
                })
                .await
                .unwrap()
        }));
    }

    let mut results = Vec::new();
    for h in handles {
        results.push(h.await.unwrap());
    }
    for p in results {
        assert_eq!(p.id, "shared");
    }

    // Factory closure must run exactly once across 5 simultaneous requests
    assert_eq!(fetch_count.load(Ordering::SeqCst), 1);
}
