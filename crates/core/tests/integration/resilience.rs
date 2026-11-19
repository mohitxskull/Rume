use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::http::StatusCode;
use rume::prelude::*;
use rume::resilience::*;

#[tokio::test]
async fn circuit_breaker_state_transitions_and_trip() {
    tokio::time::pause();
    let config = CircuitBreakerConfig {
        failure_threshold: 3,
        success_threshold: 2,
        reset_timeout: Duration::from_millis(50),
    };
    let breaker = CircuitBreaker::new(config);

    let fail_flag = Arc::new(AtomicUsize::new(0));

    let flag_clone = Arc::clone(&fail_flag);
    let router = rume::routing::Router::new()
        .get("/service", move || {
            let flag = Arc::clone(&flag_clone);
            async move {
                if flag.load(Ordering::SeqCst) == 1 {
                    (StatusCode::INTERNAL_SERVER_ERROR, "downstream error")
                } else {
                    (StatusCode::OK, "service healthy")
                }
            }
        })
        .layer(CircuitBreakerLayer::new(breaker.clone()));

    let app = TestApp::new(router);

    // Initial state: Closed
    assert_eq!(breaker.state(), CircuitState::Closed);
    let res = app.get("/service").await;
    assert_eq!(res.status(), StatusCode::OK);

    // Trigger 3 consecutive 500 errors
    fail_flag.store(1, Ordering::SeqCst);
    for _ in 0..3 {
        let res = app.get("/service").await;
        assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    // Breaker should now be OPEN
    assert_eq!(breaker.state(), CircuitState::Open);

    // Request is rejected immediately with 503 SERVICE_UNAVAILABLE
    let res = app.get("/service").await;
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    let val: serde_json::Value = serde_json::from_str(&TestApp::body_text(res).await).unwrap();
    assert_eq!(val["code"], "CIRCUIT_BREAKER_OPEN");

    // Fast-forward past reset timeout (50ms) -> transitions to HalfOpen
    tokio::time::advance(Duration::from_millis(60)).await;
    assert_eq!(breaker.state(), CircuitState::HalfOpen);

    // Restore downstream service
    fail_flag.store(0, Ordering::SeqCst);

    // 2 successes in HalfOpen close the circuit
    let res1 = app.get("/service").await;
    assert_eq!(res1.status(), StatusCode::OK);
    let res2 = app.get("/service").await;
    assert_eq!(res2.status(), StatusCode::OK);

    assert_eq!(breaker.state(), CircuitState::Closed);
}

#[tokio::test]
async fn rate_limiting_sliding_window_and_headers() {
    let limiter = RateLimiter::new(RateLimitConfig {
        max_requests: 3,
        window: Duration::from_secs(60),
        ..Default::default()
    });

    let router = rume::routing::Router::new()
        .get("/api/data", || async { "payload" })
        .layer(RateLimitLayer::new(limiter));

    let app = TestApp::new(router);

    // Request 1: 200 OK, remaining = 2
    let req1 = axum::http::Request::builder()
        .uri("/api/data")
        .header("x-forwarded-for", "192.168.1.100")
        .body(axum::body::Body::empty())
        .unwrap();
    let res1 = app.request(req1).await;
    assert_eq!(res1.status(), StatusCode::OK);
    assert_eq!(res1.headers().get("x-ratelimit-remaining").unwrap(), "2");

    // Request 2: 200 OK, remaining = 1
    let req2 = axum::http::Request::builder()
        .uri("/api/data")
        .header("x-forwarded-for", "192.168.1.100")
        .body(axum::body::Body::empty())
        .unwrap();
    let res2 = app.request(req2).await;
    assert_eq!(res2.status(), StatusCode::OK);
    assert_eq!(res2.headers().get("x-ratelimit-remaining").unwrap(), "1");

    // Request 3: 200 OK, remaining = 0
    let req3 = axum::http::Request::builder()
        .uri("/api/data")
        .header("x-forwarded-for", "192.168.1.100")
        .body(axum::body::Body::empty())
        .unwrap();
    let res3 = app.request(req3).await;
    assert_eq!(res3.status(), StatusCode::OK);
    assert_eq!(res3.headers().get("x-ratelimit-remaining").unwrap(), "0");

    // Request 4: 429 TOO_MANY_REQUESTS
    let req4 = axum::http::Request::builder()
        .uri("/api/data")
        .header("x-forwarded-for", "192.168.1.100")
        .body(axum::body::Body::empty())
        .unwrap();
    let res4 = app.request(req4).await;
    assert_eq!(res4.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(res4.headers().contains_key("retry-after"));
    let val: serde_json::Value = serde_json::from_str(&TestApp::body_text(res4).await).unwrap();
    assert_eq!(val["code"], "RATE_LIMIT_EXCEEDED");
}

#[tokio::test]
async fn concurrency_limiting_and_load_shedding() {
    let _limiter = ConcurrencyLimiter::new(2);

    let (in_flight_tx, mut in_flight_rx) = tokio::sync::mpsc::channel::<()>(2);
    let (release_tx, release_rx) = tokio::sync::watch::channel(false);

    let router = rume::routing::Router::new()
        .get("/compute", {
            let in_flight_tx = in_flight_tx.clone();
            let release_rx = release_rx.clone();
            move || {
                let tx = in_flight_tx.clone();
                let mut rx = release_rx.clone();
                async move {
                    let _ = tx.send(()).await;
                    let _ = rx.wait_for(|&released| released).await;
                    "computed"
                }
            }
        })
        .layer(ConcurrencyLimitLayer::new(2));

    let app = Arc::new(TestApp::new(router));

    // Spawn 2 long requests to saturate capacity
    let app1 = Arc::clone(&app);
    let h1 = tokio::spawn(async move { app1.get("/compute").await });

    let app2 = Arc::clone(&app);
    let h2 = tokio::spawn(async move { app2.get("/compute").await });

    // Deterministically wait until both requests have acquired their concurrency permits
    in_flight_rx.recv().await.unwrap();
    in_flight_rx.recv().await.unwrap();

    // 3rd concurrent request exceeds limit of 2 -> 503 SHED_LOAD
    let res3 = app.get("/compute").await;
    assert_eq!(res3.status(), StatusCode::SERVICE_UNAVAILABLE);
    let val: serde_json::Value = serde_json::from_str(&TestApp::body_text(res3).await).unwrap();
    assert_eq!(val["code"], "SHED_LOAD");

    // Release in-flight requests
    release_tx.send(true).unwrap();

    // Wait for in-flight requests to complete
    let (r1, r2) = tokio::join!(h1, h2);
    assert_eq!(r1.unwrap().status(), StatusCode::OK);
    assert_eq!(r2.unwrap().status(), StatusCode::OK);

    // After permits freed, next request succeeds
    let res4 = app.get("/compute").await;
    assert_eq!(res4.status(), StatusCode::OK);
}

#[tokio::test]
async fn request_timeout_middleware() {
    let router = rume::routing::Router::new()
        .get("/fast", || async { "fast" })
        .get("/slow", || async {
            // An unresolving future will exceed any timeout
            std::future::pending::<()>().await;
            "slow"
        })
        .layer(TimeoutLayer::new(Duration::from_millis(30)));

    let app = TestApp::new(router);

    // Fast request completes within 30ms -> 200 OK
    let res_fast = app.get("/fast").await;
    assert_eq!(res_fast.status(), StatusCode::OK);

    // Slow request exceeds 30ms -> 504 GATEWAY_TIMEOUT Problem Details
    let res_slow = app.get("/slow").await;
    assert_eq!(res_slow.status(), StatusCode::GATEWAY_TIMEOUT);
    let val: serde_json::Value = serde_json::from_str(&TestApp::body_text(res_slow).await).unwrap();
    assert_eq!(val["code"], "TIMEOUT");
}

#[tokio::test]
async fn test_circuit_breaker_half_open_single_trial_probe() {
    tokio::time::pause();
    let config = CircuitBreakerConfig {
        failure_threshold: 2,
        success_threshold: 2,
        reset_timeout: Duration::from_millis(40),
    };
    let breaker = CircuitBreaker::new(config);

    // Trip the breaker to Open
    breaker.on_failure();
    breaker.on_failure();
    assert_eq!(breaker.state(), CircuitState::Open);

    // Fast-forward past reset timeout -> HalfOpen
    tokio::time::advance(Duration::from_millis(50)).await;
    assert_eq!(breaker.state(), CircuitState::HalfOpen);

    // First request is allowed as trial probe
    let permit1 = breaker
        .try_acquire()
        .expect("first request allowed as trial probe");

    // Concurrent second request during probe must be rejected!
    assert!(breaker.try_acquire().is_none());

    // Once probe succeeds, subsequent trial can proceed
    permit1.complete_success();
    let permit2 = breaker
        .try_acquire()
        .expect("second trial allowed after first succeeds");
    permit2.complete_success();

    // Closed after 2 successes
    assert_eq!(breaker.state(), CircuitState::Closed);
}

#[tokio::test]
async fn test_circuit_breaker_half_open_abandoned_trial_released() {
    tokio::time::pause();
    let config = CircuitBreakerConfig {
        failure_threshold: 2,
        success_threshold: 2,
        reset_timeout: Duration::from_millis(40),
    };
    let breaker = CircuitBreaker::new(config);

    // Trip the breaker to Open
    breaker.on_failure();
    breaker.on_failure();
    assert_eq!(breaker.state(), CircuitState::Open);

    // Fast-forward past reset timeout -> HalfOpen
    tokio::time::advance(Duration::from_millis(50)).await;
    assert_eq!(breaker.state(), CircuitState::HalfOpen);

    // First request is allowed as trial probe
    let permit = breaker
        .try_acquire()
        .expect("first request allowed as trial probe");

    // Abandon the trial: drop the permit without calling complete_success/complete_failure
    // (simulates client disconnect or future cancellation). The half_open_in_flight slot
    // is released immediately by TrialPermit::drop, never wedging the circuit.
    drop(permit);

    // A subsequent trial must be permitted (not wedged HalfOpen)
    assert!(
        breaker.try_acquire().is_some(),
        "abandoned HalfOpen trial should release the slot, not wedge the circuit"
    );
}

#[tokio::test]
async fn test_rate_limiter_prune_expired_clients() {
    tokio::time::pause();
    let limiter = RateLimiter::new(RateLimitConfig {
        max_requests: 5,
        window: Duration::from_millis(30),
        ..Default::default()
    });

    assert!(limiter.check("client_temp_1").is_ok());
    assert!(limiter.check("client_temp_2").is_ok());

    // Fast-forward past sliding window
    tokio::time::advance(Duration::from_millis(40)).await;

    // Pruning should clean up inactive entries
    limiter.prune_expired();

    // Fresh requests start with full quota
    let status = limiter.check("client_temp_1").unwrap();
    assert_eq!(status.remaining, 4);
}

#[tokio::test]
async fn circuit_breaker_trips_on_service_transport_errors() {
    use axum::body::Body;
    use axum::http::{Request, Response};
    use tower::Service;
    use tower::service_fn;

    let config = CircuitBreakerConfig {
        failure_threshold: 2,
        success_threshold: 1,
        reset_timeout: Duration::from_secs(60),
    };
    let breaker = CircuitBreaker::new(config);

    let failing_service = service_fn(|_req: Request<Body>| async {
        Err::<Response<Body>, _>(std::io::Error::new(
            std::io::ErrorKind::ConnectionReset,
            "transport drop",
        ))
    });

    let mut cb_service = CircuitBreakerService::new(failing_service, breaker.clone());

    assert_eq!(breaker.state(), CircuitState::Closed);

    let req1 = Request::builder().body(Body::empty()).unwrap();
    let _ = cb_service.call(req1).await;
    assert_eq!(breaker.state(), CircuitState::Closed);

    let req2 = Request::builder().body(Body::empty()).unwrap();
    let _ = cb_service.call(req2).await;
    assert_eq!(breaker.state(), CircuitState::Open);
}

#[tokio::test]
async fn test_tenant_extension_rate_limiting() {
    use rume::tenant::Tenant;

    let config = RateLimitConfig {
        max_requests: 2,
        window: Duration::from_secs(60),
        ..Default::default()
    };
    let limiter = RateLimiter::new(config);

    let router = rume::routing::Router::new()
        .get("/tenant-api", || async { "ok" })
        .layer(RateLimitLayer::new(limiter));

    let app = TestApp::new(router);

    // Tenant 1 request 1
    let mut req1 = axum::http::Request::builder()
        .uri("/tenant-api")
        .body(axum::body::Body::empty())
        .unwrap();
    req1.extensions_mut()
        .insert(Tenant::new("tenant_123", "acme"));
    let res1 = app.request(req1).await;
    assert_eq!(res1.status(), StatusCode::OK);

    // Tenant 1 request 2
    let mut req2 = axum::http::Request::builder()
        .uri("/tenant-api")
        .body(axum::body::Body::empty())
        .unwrap();
    req2.extensions_mut()
        .insert(Tenant::new("tenant_123", "acme"));
    let res2 = app.request(req2).await;
    assert_eq!(res2.status(), StatusCode::OK);

    // Tenant 1 request 3 -> Rate limit exceeded
    let mut req3 = axum::http::Request::builder()
        .uri("/tenant-api")
        .body(axum::body::Body::empty())
        .unwrap();
    req3.extensions_mut()
        .insert(Tenant::new("tenant_123", "acme"));
    let res3 = app.request(req3).await;
    assert_eq!(res3.status(), StatusCode::TOO_MANY_REQUESTS);

    // Tenant 2 should NOT be rate limited
    let mut req_t2 = axum::http::Request::builder()
        .uri("/tenant-api")
        .body(axum::body::Body::empty())
        .unwrap();
    req_t2
        .extensions_mut()
        .insert(Tenant::new("tenant_456", "beta"));
    let res_t2 = app.request(req_t2).await;
    assert_eq!(res_t2.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_rate_limiter_overflow_bucket_preserves_active_clients() {
    use std::hash::{Hash, Hasher};

    fn key_for_shard(shard_target: usize, prefix: &str) -> String {
        for i in 0..10_000 {
            let candidate = format!("{prefix}_{i}");
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            candidate.hash(&mut hasher);
            if ((hasher.finish() as usize) % 16) == shard_target {
                return candidate;
            }
        }
        panic!("could not find key for shard");
    }

    let config = RateLimitConfig::default()
        .with_max_requests(1)
        .with_window(Duration::from_secs(60))
        .with_max_entries_per_shard(2);
    let limiter = RateLimiter::new(config);

    // Pick 4 keys that are guaranteed to map to the exact same shard
    let key_a = key_for_shard(3, "client_a");
    let key_b = key_for_shard(3, "client_b");
    let key_c = key_for_shard(3, "client_c");
    let key_d = key_for_shard(3, "client_d");

    // 1. client_A exhausts its rate limit quota
    assert!(limiter.check(&key_a).is_ok());
    assert!(
        limiter.check(&key_a).is_err(),
        "second request for client_A must be rate limited"
    );

    // 2. client_B exhausts its rate limit quota; shard now has 2 entries (full capacity)
    assert!(limiter.check(&key_b).is_ok());
    assert!(
        limiter.check(&key_b).is_err(),
        "second request for client_B must be rate limited"
    );

    // 3. client_C arrives when shard is at capacity.
    // Rather than evicting client_A, client_C is routed to the shared __overflow__ bucket.
    assert!(
        limiter.check(&key_c).is_ok(),
        "client_C uses first request of __overflow__ bucket"
    );

    // 4. client_D also arrives; since __overflow__ bucket already consumed its 1 request, client_D is limited.
    assert!(
        limiter.check(&key_d).is_err(),
        "client_D must be rate limited by saturated __overflow__ bucket"
    );

    // 5. Critical security invariant: client_A was NOT evicted, so it CANNOT bypass rate limiting!
    assert!(
        limiter.check(&key_a).is_err(),
        "client_A must remain rate limited; new keys cannot evict active clients"
    );
}

#[test]
fn test_resolve_forwarded_client_ip_chain_security() {
    use rume::resilience::rate_limiter::resolve_forwarded_client_ip;
    use std::net::IpAddr;

    let trusted_proxy_1: IpAddr = "10.0.0.1".parse().unwrap();
    let trusted_proxy_2: IpAddr = "10.0.0.2".parse().unwrap();
    let trusted = vec![trusted_proxy_1, trusted_proxy_2];

    let untrusted_peer: IpAddr = "198.51.100.5".parse().unwrap();
    let real_client: IpAddr = "203.0.113.195".parse().unwrap();

    // 1. Untrusted peer: ignores X-Forwarded-For completely
    let spoofed = "1.1.1.1, 8.8.8.8";
    let res = resolve_forwarded_client_ip(Some(untrusted_peer), Some(spoofed), &trusted);
    assert_eq!(res, format!("ip:{untrusted_peer}"));

    // 2. Missing socket peer
    let res_none = resolve_forwarded_client_ip(None, Some(spoofed), &trusted);
    assert_eq!(res_none, "client_default");

    // 3. Trusted immediate proxy, single client in chain
    let res_single =
        resolve_forwarded_client_ip(Some(trusted_proxy_1), Some("203.0.113.195"), &trusted);
    assert_eq!(res_single, format!("ip:{real_client}"));

    // 4. Trusted immediate proxy, intermediate trusted proxy, untrusted client, spoofed prefix
    // Header format: <spoofed_attacker_ip>, <real_client_ip>, <intermediate_trusted_proxy>
    let complex_chain = "1.1.1.1, 203.0.113.195, 10.0.0.2";
    let res_chain =
        resolve_forwarded_client_ip(Some(trusted_proxy_1), Some(complex_chain), &trusted);
    assert_eq!(
        res_chain,
        format!("ip:{real_client}"),
        "must pick the first untrusted IP from the right, ignoring spoofed leftmost IP"
    );

    // 5. All hops in chain are trusted proxies
    let all_trusted_chain = "10.0.0.2, 10.0.0.1";
    let res_all_trusted =
        resolve_forwarded_client_ip(Some(trusted_proxy_1), Some(all_trusted_chain), &trusted);
    assert_eq!(res_all_trusted, "ip:10.0.0.2");
}

#[tokio::test]
async fn test_rate_limiter_ignores_unauthenticated_tenant_header() {
    let config = RateLimitConfig::default()
        .with_max_requests(2)
        .with_window(Duration::from_secs(60));
    let limiter = RateLimiter::new(config);

    let router = rume::routing::Router::new()
        .get("/api/unauth", || async { "ok" })
        .layer(RateLimitLayer::new(limiter));

    let app = TestApp::new(router);

    // Request 1 without ConnectInfo uses client_default
    let req1 = axum::http::Request::builder()
        .uri("/api/unauth")
        .header("x-tenant-id", "attacker_tenant_1")
        .body(axum::body::Body::empty())
        .unwrap();
    let res1 = app.request(req1).await;
    assert_eq!(res1.status(), StatusCode::OK);

    // Request 2 with different x-tenant-id STILL shares the client bucket because x-tenant-id is unauthenticated
    let req2 = axum::http::Request::builder()
        .uri("/api/unauth")
        .header("x-tenant-id", "attacker_tenant_2")
        .body(axum::body::Body::empty())
        .unwrap();
    let res2 = app.request(req2).await;
    assert_eq!(res2.status(), StatusCode::OK);

    // Request 3 is rejected: client_default limit exhausted! Forged tenant header was not honored.
    let req3 = axum::http::Request::builder()
        .uri("/api/unauth")
        .header("x-tenant-id", "attacker_tenant_3")
        .body(axum::body::Body::empty())
        .unwrap();
    let res3 = app.request(req3).await;
    assert_eq!(res3.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn test_rate_limiter_with_tenant_layer_scopes_anonymous_requests_by_client_ip() {
    tokio::time::pause();

    use axum::extract::ConnectInfo;
    use rume::auth::identity::AuthUser;
    use rume::tenant::TenantLayer;
    use rume::tenant::resolver::HeaderTenantResolver;
    use std::net::SocketAddr;

    let config = RateLimitConfig {
        max_requests: 2,
        window: Duration::from_secs(60),
        ..Default::default()
    };
    let limiter = RateLimiter::new(config);

    // Stack TenantLayer::optional(HeaderTenantResolver) outside RateLimitLayer.
    // Also register a second tenant whose ID ends with IP A's client key suffix
    // (`acme:198.51.100.10`) to prove anonymous traffic to `acme` from IP A cannot
    // collide with the authenticated bucket of tenant `acme:198.51.100.10`.
    let resolver = HeaderTenantResolver::default();
    resolver
        .try_add_tenant(rume::tenant::Tenant::new("acme", "acme"))
        .unwrap();
    resolver
        .try_add_tenant(rume::tenant::Tenant::new(
            "acme:198.51.100.10",
            "acme-ip-suffix",
        ))
        .unwrap();
    let router = rume::routing::Router::new()
        .get("/api/tenant-scoped", || async { "ok" })
        .layer(RateLimitLayer::new(limiter))
        .layer(TenantLayer::optional(resolver));

    let app = TestApp::new(router);
    let ip_a: SocketAddr = "198.51.100.10:40000".parse().unwrap();
    let ip_b: SocketAddr = "198.51.100.20:40000".parse().unwrap();

    // Anonymous IP A sends max_requests (2) with X-Tenant-ID: acme -> both succeed
    for _ in 0..2 {
        let mut req = axum::http::Request::builder()
            .uri("/api/tenant-scoped")
            .header("x-tenant-id", "acme")
            .body(axum::body::Body::empty())
            .unwrap();
        req.extensions_mut().insert(ConnectInfo(ip_a));
        let res = app.request(req).await;
        assert_eq!(res.status(), StatusCode::OK);
    }

    // 3rd anonymous request from IP A with X-Tenant-ID: acme is rate-limited (429)
    let mut req_a_overflow = axum::http::Request::builder()
        .uri("/api/tenant-scoped")
        .header("x-tenant-id", "acme")
        .body(axum::body::Body::empty())
        .unwrap();
    req_a_overflow.extensions_mut().insert(ConnectInfo(ip_a));
    let res_a_overflow = app.request(req_a_overflow).await;
    assert_eq!(res_a_overflow.status(), StatusCode::TOO_MANY_REQUESTS);

    // Anonymous request from IP B for the same tenant (X-Tenant-ID: acme) still succeeds!
    let mut req_b = axum::http::Request::builder()
        .uri("/api/tenant-scoped")
        .header("x-tenant-id", "acme")
        .body(axum::body::Body::empty())
        .unwrap();
    req_b.extensions_mut().insert(ConnectInfo(ip_b));
    let res_b = app.request(req_b).await;
    assert_eq!(
        res_b.status(),
        StatusCode::OK,
        "anonymous client IP A must not drain tenant 'acme' rate-limit bucket for IP B"
    );

    // Authenticated member of tenant `acme:198.51.100.10` must NOT share a bucket with
    // anonymous requests to `acme` from `198.51.100.10`.
    let mut req_auth_suffix = axum::http::Request::builder()
        .uri("/api/tenant-scoped")
        .header("x-tenant-id", "acme:198.51.100.10")
        .body(axum::body::Body::empty())
        .unwrap();
    req_auth_suffix
        .extensions_mut()
        .insert(AuthUser::new("member-1").with_tenant_id("acme:198.51.100.10"));
    let res_auth_suffix = app.request(req_auth_suffix).await;
    assert_eq!(
        res_auth_suffix.status(),
        StatusCode::OK,
        "anonymous traffic to 'acme' from 198.51.100.10 must not collide with authenticated tenant 'acme:198.51.100.10'"
    );
}

/// Regression test for RC-08: `RateLimiter::check` must not panic on a `current_thread`
/// runtime when a Redis backend is configured. It must return a clear error directing
/// callers to `check_async` instead of blocking on the async Redis client via
/// `block_in_place` + `Handle::current().block_on`.
///
/// The pool is never actually used: `check` returns an error before touching Redis, and
/// pool construction is lazy (no connection is opened).
#[tokio::test(flavor = "current_thread")]
#[cfg(feature = "redis")]
async fn rate_limiter_check_redis_returns_backend_error_on_current_thread_runtime() {
    use deadpool_redis::{Manager, Pool};

    let pool = Pool::builder(Manager::new("redis://127.0.0.1:6379").unwrap())
        .build()
        .unwrap();

    let config = RateLimitConfig {
        max_requests: 10,
        window: Duration::from_secs(60),
        ..Default::default()
    };
    let limiter = RateLimiter::redis(pool, config);

    // Runs inside a `current_thread` runtime: before the RC-08 fix this panicked with
    // "cannot block on a runtime from within a runtime".
    let result = limiter.check("some-key");

    assert!(
        matches!(result, Err(ResilienceError::Backend(_))),
        "expected Backend error directing callers to check_async, got: {result:?}"
    );
}
