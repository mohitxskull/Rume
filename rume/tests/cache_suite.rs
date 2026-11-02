use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use rume::cache::{Cache, CacheConfig, MemoryCacheBackend};
use rume::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct Product {
    id: String,
    title: String,
    price_cents: u64,
}

#[tokio::test]
async fn cache_crud_and_prefix_namespaces() {
    let config = CacheConfig::new("memory://")
        .with_default_ttl(60)
        .with_prefix("app:v1:");

    let cache = Cache::new(MemoryCacheBackend::new(), config);

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
async fn cache_ttl_expiration() {
    let cache = Cache::memory();

    let product = Product {
        id: "prod_ttl".into(),
        title: "Fast Item".into(),
        price_cents: 1000,
    };

    // Set with short TTL: 20ms
    cache
        .set_json("prod_ttl", &product, Some(Duration::from_millis(20)))
        .await
        .unwrap();

    // Immediately available
    assert!(cache.exists("prod_ttl").await.unwrap());

    // Sleep past TTL
    tokio::time::sleep(Duration::from_millis(30)).await;

    // Now expired and returns None
    assert!(!cache.exists("prod_ttl").await.unwrap());
    assert_eq!(cache.get_json::<Product>("prod_ttl").await.unwrap(), None);
}

#[tokio::test]
async fn cache_tag_invalidation() {
    let cache = Cache::memory();

    let p1 = Product {
        id: "1".into(),
        title: "Book A".into(),
        price_cents: 100,
    };
    let p2 = Product {
        id: "2".into(),
        title: "Book B".into(),
        price_cents: 200,
    };
    let p3 = Product {
        id: "3".into(),
        title: "Shoes".into(),
        price_cents: 500,
    };

    // Tag p1 and p2 with "books" and "catalog"
    cache
        .set_json_with_tags("item:1", &p1, &["books", "catalog"], None)
        .await
        .unwrap();
    cache
        .set_json_with_tags("item:2", &p2, &["books", "catalog"], None)
        .await
        .unwrap();
    // Tag p3 with "apparel" and "catalog"
    cache
        .set_json_with_tags("item:3", &p3, &["apparel", "catalog"], None)
        .await
        .unwrap();

    assert!(cache.exists("item:1").await.unwrap());
    assert!(cache.exists("item:2").await.unwrap());
    assert!(cache.exists("item:3").await.unwrap());

    // Invalidate "books" tag: should remove item:1 and item:2, but preserve item:3
    let evicted = cache.invalidate_tag("books").await.unwrap();
    assert_eq!(evicted, 2);

    assert!(!cache.exists("item:1").await.unwrap());
    assert!(!cache.exists("item:2").await.unwrap());
    assert!(cache.exists("item:3").await.unwrap());

    // Invalidate "catalog" tag: should remove item:3
    let evicted2 = cache.invalidate_tag("catalog").await.unwrap();
    assert_eq!(evicted2, 1);
    assert!(!cache.exists("item:3").await.unwrap());
}

#[tokio::test]
async fn cache_remember_cache_aside_pattern() {
    let cache = Cache::memory();
    let compute_counter = Arc::new(AtomicUsize::new(0));

    let counter_clone = Arc::clone(&compute_counter);
    let fetch_product = move || {
        let cnt = Arc::clone(&counter_clone);
        async move {
            cnt.fetch_add(1, Ordering::SeqCst);
            Ok(Product {
                id: "computed_1".into(),
                title: "Computed Product".into(),
                price_cents: 999,
            })
        }
    };

    // 1st call: cache miss, runs compute closure
    let p1: Product = cache
        .remember("key:prod", None, fetch_product)
        .await
        .unwrap();
    assert_eq!(p1.id, "computed_1");
    assert_eq!(compute_counter.load(Ordering::SeqCst), 1);

    // 2nd call: cache hit, closure not executed!
    let counter_clone2 = Arc::clone(&compute_counter);
    let p2: Product = cache
        .remember("key:prod", None, move || async move {
            counter_clone2.fetch_add(1, Ordering::SeqCst);
            Ok(Product {
                id: "other".into(),
                title: "Should Not Run".into(),
                price_cents: 0,
            })
        })
        .await
        .unwrap();

    assert_eq!(p2.id, "computed_1");
    assert_eq!(compute_counter.load(Ordering::SeqCst), 1); // Counter did not increment!
}

#[tokio::test]
async fn cache_health_check_reports_diagnostics() {
    let backend = MemoryCacheBackend::new();
    let cache = Cache::new(backend.clone(), CacheConfig::default());
    let hc = cache.health_check("app_cache");

    // Initially healthy
    let res = hc.run().await;
    assert_eq!(res.status, HealthStatus::Ok);
    assert_eq!(res.name, "app_cache");
    assert_eq!(
        res.meta.get("is_closed").unwrap(),
        &serde_json::json!(false)
    );

    // Simulate backend offline
    backend.set_alive(false);
    let res_offline = hc.run().await;
    assert_eq!(res_offline.status, HealthStatus::Failed);
    assert!(
        res_offline
            .message
            .contains("cache connection check failed")
    );

    // Recover
    backend.set_alive(true);
    let res_ok = hc.run().await;
    assert_eq!(res_ok.status, HealthStatus::Ok);

    // Closed cache
    cache.close();
    let res_closed = hc.run().await;
    assert_eq!(res_closed.status, HealthStatus::Failed);
    assert_eq!(res_closed.message, "cache is closed");
}

#[tokio::test]
async fn cache_remember_stampede_singleflight_protection() {
    let cache = Arc::new(Cache::memory());
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

#[tokio::test]
async fn memory_cache_backend_prune_and_capacity_bound() {
    let backend = MemoryCacheBackend::with_capacity(3);
    let cache = Cache::new(backend.clone(), CacheConfig::default());

    // Insert 3 items
    cache.set_json("k1", &10, None).await.unwrap();
    cache.set_json("k2", &20, None).await.unwrap();
    cache.set_json("k3", &30, None).await.unwrap();
    assert_eq!(backend.len(), 3);

    // Insert 4th item -> capacity bounded to 3 via eviction of oldest
    cache.set_json("k4", &40, None).await.unwrap();
    assert_eq!(backend.len(), 3);

    // Insert short-lived item
    cache
        .set_json("k_temp", &99, Some(Duration::from_millis(20)))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await;

    // Prune drops expired
    backend.prune_expired();
    assert!(!cache.exists("k_temp").await.unwrap());
}
