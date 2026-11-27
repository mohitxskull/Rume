//! Criterion micro-benchmarks for Rume's hot HTTP and resilience paths:
//! 1. `Router` dispatch + `ObservabilityLayer` (`W3C TraceContext` + `RequestId`)
//! 2. `RateLimiter::check` (in-memory sliding window)
//! 3. `CircuitBreaker::try_acquire` + `complete_success`
//! 4. `TenantScopedCache::get` / `set` (in-memory backend + quota store)

#![forbid(unsafe_code)]

use std::hint::black_box;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::Request;
use axum::routing::get;
use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use rume::app::ObservabilityLayer;
use rume::cache::Cache;
use rume::cache::backend::InMemoryCacheBackend;
use rume::cache::config::CacheConfig;
use rume::resilience::{CircuitBreaker, CircuitBreakerConfig, RateLimitConfig, RateLimiter};
use rume::tenant::cache::TenantScopedCache;
use rume::tenant::quota::InMemoryTenantQuotaStore;
use tower::ServiceExt;

fn bench_router_observability_dispatch(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    let raw_axum = axum::Router::new().route("/ping", get(|| async { "pong" }));
    let rume_with_obs = axum::Router::new()
        .route("/ping", get(|| async { "pong" }))
        .layer(ObservabilityLayer::new());

    c.bench_function("hot_path/raw_axum_router", |b| {
        b.to_async(&rt).iter(|| {
            let app = raw_axum.clone();
            async move {
                let req = Request::builder()
                    .uri("/ping")
                    .header(
                        "traceparent",
                        "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
                    )
                    .body(Body::empty())
                    .expect("valid request");
                let resp = app.oneshot(req).await.expect("infallible response");
                black_box(resp.status());
            }
        });
    });

    c.bench_function("hot_path/router_plus_observability_layer", |b| {
        b.to_async(&rt).iter(|| {
            let app = rume_with_obs.clone();
            async move {
                let req = Request::builder()
                    .uri("/ping")
                    .header(
                        "traceparent",
                        "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
                    )
                    .body(Body::empty())
                    .expect("valid request");
                let resp = app.oneshot(req).await.expect("infallible response");
                black_box(resp.status());
            }
        });
    });
}

fn bench_rate_limiter_check(c: &mut Criterion) {
    c.bench_function("hot_path/rate_limiter_in_memory_check", |b| {
        b.iter_batched_ref(
            || {
                let limiter = RateLimiter::new(RateLimitConfig {
                    max_requests: 1_000,
                    window: Duration::from_secs(60),
                    max_entries_per_shard: 10_000,
                    trusted_proxies: Vec::new(),
                });
                // Seed a steady-state history of 32 prior requests in the active window.
                for _ in 0..32 {
                    limiter
                        .check("tenant:bench-1")
                        .expect("seed within steady-state limit");
                }
                limiter
            },
            |limiter| {
                let status = limiter.check(black_box("tenant:bench-1"));
                black_box(status).expect("within limit");
            },
            BatchSize::SmallInput,
        );
    });
}

fn bench_circuit_breaker_closed_fast_path(c: &mut Criterion) {
    let breaker = CircuitBreaker::new(CircuitBreakerConfig {
        failure_threshold: 5,
        success_threshold: 2,
        reset_timeout: Duration::from_secs(30),
        ..Default::default()
    });

    c.bench_function("hot_path/circuit_breaker_try_acquire_closed", |b| {
        b.iter(|| {
            let permit = breaker
                .try_acquire()
                .expect("closed breaker admits requests");
            permit.complete_success();
            black_box(());
        });
    });
}

fn bench_tenant_scoped_cache(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    let cache = Cache::new(InMemoryCacheBackend::new(), CacheConfig::default());
    let quota = Arc::new(InMemoryTenantQuotaStore::new());
    let tenant_cache = TenantScopedCache::new(cache)
        .with_max_keys_per_tenant(1_000)
        .with_quota_store(quota);

    rt.block_on(async {
        tenant_cache
            .set(
                "tenant-1",
                "hot-key",
                &"cached-payload",
                Some(Duration::from_secs(300)),
            )
            .await
            .expect("seed hot key");
    });

    c.bench_function("hot_path/tenant_scoped_cache_get_hit", |b| {
        b.to_async(&rt).iter(|| async {
            let val: String = tenant_cache
                .get(black_box("tenant-1"), black_box("hot-key"))
                .await
                .expect("cache get")
                .expect("seeded hot-key must remain a cache hit");
            black_box(val);
        });
    });

    c.bench_function("hot_path/tenant_scoped_cache_set_overwrite", |b| {
        b.to_async(&rt).iter(|| async {
            tenant_cache
                .set(
                    black_box("tenant-1"),
                    black_box("hot-key"),
                    black_box(&"updated-payload"),
                    Some(Duration::from_secs(300)),
                )
                .await
                .expect("cache set");
        });
    });
}

fn bench_singleflight_cache_contention(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("tokio runtime");

    c.bench_function("hot_path/singleflight_cache_64_concurrent_tasks", |b| {
        b.to_async(&rt).iter_batched(
            || {
                Arc::new(Cache::new(
                    InMemoryCacheBackend::new(),
                    CacheConfig::default(),
                ))
            },
            |cache| async move {
                let mut handles = Vec::with_capacity(64);
                for _ in 0..64 {
                    let cache = Arc::clone(&cache);
                    handles.push(tokio::spawn(async move {
                        cache
                            .remember("contended-key", Some(Duration::from_secs(60)), || async {
                                Ok::<_, rume::cache::CacheError>("computed-value".to_string())
                            })
                            .await
                            .expect("singleflight remember")
                    }));
                }
                for handle in handles {
                    let val = handle.await.expect("task join");
                    black_box(val);
                }
            },
            BatchSize::SmallInput,
        );
    });
}

criterion_group!(
    hot_path_benches,
    bench_router_observability_dispatch,
    bench_rate_limiter_check,
    bench_circuit_breaker_closed_fast_path,
    bench_tenant_scoped_cache,
    bench_singleflight_cache_contention,
);
criterion_main!(hot_path_benches);
