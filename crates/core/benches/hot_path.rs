//! Criterion micro-benchmarks for Rume's hot HTTP and resilience paths:
//! 1. `Router` dispatch + `ObservabilityLayer` (`W3C TraceContext` + `RequestId`)
//! 2. `RateLimiter::check` (in-memory sliding window)
//! 3. `CircuitBreaker::try_acquire` + `complete_success`
//! 4. `TenantScopedCache::get` / `set` (in-memory backend + quota store)

use std::hint::black_box;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::Request;
use axum::routing::get;
use criterion::{Criterion, criterion_group, criterion_main};
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

    let app = axum::Router::new()
        .route("/ping", get(|| async { "pong" }))
        .layer(ObservabilityLayer::new());

    c.bench_function("hot_path/router_plus_observability_layer", |b| {
        b.to_async(&rt).iter(|| {
            let app = app.clone();
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
    let limiter = RateLimiter::new(RateLimitConfig {
        max_requests: u32::MAX,
        window: Duration::from_secs(60),
        max_entries_per_shard: 10_000,
        trusted_proxies: Vec::new(),
    });

    c.bench_function("hot_path/rate_limiter_in_memory_check", |b| {
        b.iter(|| {
            let status = limiter.check(black_box("tenant:bench-1"));
            black_box(status).expect("within limit");
        });
    });
}

fn bench_circuit_breaker_closed_fast_path(c: &mut Criterion) {
    let breaker = CircuitBreaker::new(CircuitBreakerConfig {
        failure_threshold: 5,
        success_threshold: 2,
        reset_timeout: Duration::from_secs(30),
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
            let val: Option<String> = tenant_cache
                .get(black_box("tenant-1"), black_box("hot-key"))
                .await
                .expect("cache get");
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

criterion_group!(
    hot_path_benches,
    bench_router_observability_dispatch,
    bench_rate_limiter_check,
    bench_circuit_breaker_closed_fast_path,
    bench_tenant_scoped_cache,
);
criterion_main!(hot_path_benches);
