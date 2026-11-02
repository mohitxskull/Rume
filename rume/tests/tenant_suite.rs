use std::sync::Arc;
use axum::http::{Request, StatusCode};
use axum::response::IntoResponse;
use axum::routing::get;
use rume::cache::{Cache, CacheConfig, RedisCacheBackend};
use rume::prelude::*;
use rume::tenant::*;
use testcontainers_modules::redis::Redis;
use testcontainers_modules::testcontainers::runners::AsyncRunner;

#[tokio::test]
async fn test_header_tenant_resolver_success_and_context_extractor() {
    let resolver = HeaderTenantResolver::new().add_tenant(Tenant::new("tenant-123", "acme"));

    let app = Router::new()
        .route(
            "/tenant/info",
            get(|TenantContext(tenant): TenantContext| async move {
                Json(serde_json::json!({
                    "id": tenant.id,
                    "slug": tenant.slug,
                }))
            }),
        )
        .layer(TenantLayer::required(resolver));

    let test_app = TestApp::new(app);

    let req = Request::builder()
        .uri("/tenant/info")
        .header("x-tenant-id", "tenant-123")
        .body(axum::body::Body::empty())
        .unwrap();

    let res = test_app.request(req).await;
    assert_eq!(res.status(), StatusCode::OK);
    let body: serde_json::Value = serde_json::from_str(&TestApp::body_text(res).await).unwrap();
    assert_eq!(body["id"], "tenant-123");
    assert_eq!(body["slug"], "acme");
}

#[tokio::test]
async fn test_tenant_missing_and_unknown_rejections() {
    let resolver = HeaderTenantResolver::new().add_tenant(Tenant::new("tenant-123", "acme"));

    let app = Router::new()
        .route(
            "/tenant/info",
            get(|TenantContext(tenant): TenantContext| async move { tenant.id.into_response() }),
        )
        .layer(TenantLayer::required(resolver));

    let test_app = TestApp::new(app);

    // Missing tenant
    let req = Request::builder()
        .uri("/tenant/info")
        .body(axum::body::Body::empty())
        .unwrap();

    let res = test_app.request(req).await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let body: serde_json::Value = serde_json::from_str(&TestApp::body_text(res).await).unwrap();
    assert_eq!(body["code"], "MISSING_TENANT");

    // Unknown tenant
    let req = Request::builder()
        .uri("/tenant/info")
        .header("x-tenant-id", "non-existent")
        .body(axum::body::Body::empty())
        .unwrap();

    let res = test_app.request(req).await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let body: serde_json::Value = serde_json::from_str(&TestApp::body_text(res).await).unwrap();
    assert_eq!(body["code"], "TENANT_NOT_FOUND");
}

#[tokio::test]
async fn test_subdomain_tenant_resolver() {
    let resolver =
        SubdomainTenantResolver::new("app.platform.io").add_tenant(Tenant::new("t-456", "globex"));

    let app = Router::new()
        .route(
            "/whoami",
            get(|TenantContext(tenant): TenantContext| async move { tenant.slug.into_response() }),
        )
        .layer(TenantLayer::required(resolver));

    let test_app = TestApp::new(app);

    // Valid subdomain
    let req = Request::builder()
        .uri("/whoami")
        .header("host", "globex.app.platform.io:8080")
        .body(axum::body::Body::empty())
        .unwrap();

    let res = test_app.request(req).await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(TestApp::body_text(res).await, "globex");

    // Suffix spoofing attempt (evilapp.platform.io instead of .app.platform.io)
    let req_spoof = Request::builder()
        .uri("/whoami")
        .header("host", "evilglobexapp.platform.io")
        .body(axum::body::Body::empty())
        .unwrap();
    let res = test_app.request(req_spoof).await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    // Multi-level internal dot subdomain
    let req_multi = Request::builder()
        .uri("/whoami")
        .header("host", "sub.globex.app.platform.io")
        .body(axum::body::Body::empty())
        .unwrap();
    let res = test_app.request(req_multi).await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_header_tenant_resolver_id_and_slug_namespace_isolation() {
    // Tenant 1 has slug "acme" with id "t-100"
    // Tenant 2 has id "acme" with slug "globex"
    let resolver = HeaderTenantResolver::new()
        .add_tenant(Tenant::new("t-100", "acme"))
        .add_tenant(Tenant::new("acme", "globex"));

    let app = Router::new()
        .route(
            "/tenant",
            get(|TenantContext(tenant): TenantContext| async move {
                Json(serde_json::json!({
                    "id": tenant.id,
                    "slug": tenant.slug,
                }))
            }),
        )
        .layer(TenantLayer::required(resolver));

    let test_app = TestApp::new(app);

    // Request with x-tenant-id: acme should match Tenant 2 by ID first
    let req = Request::builder()
        .uri("/tenant")
        .header("x-tenant-id", "acme")
        .body(axum::body::Body::empty())
        .unwrap();
    let res = test_app.request(req).await;
    assert_eq!(res.status(), StatusCode::OK);
    let body: serde_json::Value = serde_json::from_str(&TestApp::body_text(res).await).unwrap();
    assert_eq!(body["id"], "acme");
    assert_eq!(body["slug"], "globex");

    // Request with x-tenant-id: t-100 matches Tenant 1 by ID
    let req = Request::builder()
        .uri("/tenant")
        .header("x-tenant-id", "t-100")
        .body(axum::body::Body::empty())
        .unwrap();
    let res = test_app.request(req).await;
    assert_eq!(res.status(), StatusCode::OK);
    let body: serde_json::Value = serde_json::from_str(&TestApp::body_text(res).await).unwrap();
    assert_eq!(body["id"], "t-100");
    assert_eq!(body["slug"], "acme");
}

#[tokio::test]
async fn test_optional_tenant_extractor() {
    let resolver = HeaderTenantResolver::new().add_tenant(Tenant::new("t-789", "umbrella"));

    let app = Router::new()
        .route(
            "/public-or-tenant",
            get(|OptionalTenant(tenant): OptionalTenant| async move {
                match tenant {
                    Some(t) => format!("tenant: {}", t.slug),
                    None => "global".to_string(),
                }
            }),
        )
        .layer(TenantLayer::optional(resolver));

    let test_app = TestApp::new(app);

    // Request without tenant
    let req = Request::builder()
        .uri("/public-or-tenant")
        .body(axum::body::Body::empty())
        .unwrap();
    let res = test_app.request(req).await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(TestApp::body_text(res).await, "global");

    // Request with tenant
    let req = Request::builder()
        .uri("/public-or-tenant")
        .header("x-tenant-id", "umbrella")
        .body(axum::body::Body::empty())
        .unwrap();
    let res = test_app.request(req).await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(TestApp::body_text(res).await, "tenant: umbrella");
}

async fn start_redis_tenant_cache() -> (
    testcontainers_modules::testcontainers::ContainerAsync<Redis>,
    TenantScopedCache<RedisCacheBackend>,
) {
    let node = Redis::default().start().await.expect("start redis node");
    let port = node
        .get_host_port_ipv4(6379)
        .await
        .expect("get redis port");
    let url = format!("redis://127.0.0.1:{port}");
    let pool_cfg = deadpool_redis::Config::from_url(&url);
    let pool = pool_cfg
        .create_pool(Some(deadpool_redis::Runtime::Tokio1))
        .expect("create redis pool");
    let backend = RedisCacheBackend::new(&url).expect("create redis cache backend");
    let base_cache = Cache::new(backend, CacheConfig::new(&url));
    let quota_store = Arc::new(RedisTenantQuotaStore::new(pool));
    let scoped = TenantScopedCache::new(base_cache).with_quota_store(quota_store);
    (node, scoped)
}

#[tokio::test]
async fn test_tenant_scoped_cache() {
    let (_node, scoped_cache) = start_redis_tenant_cache().await;

    #[derive(Serialize, Deserialize, PartialEq, Debug)]
    struct UserSettings {
        theme: String,
    }

    // Set for tenant-A
    scoped_cache
        .set(
            "tenant-a",
            "settings",
            &UserSettings {
                theme: "dark".into(),
            },
            None,
        )
        .await
        .unwrap();

    // Set for tenant-B
    scoped_cache
        .set(
            "tenant-b",
            "settings",
            &UserSettings {
                theme: "light".into(),
            },
            None,
        )
        .await
        .unwrap();

    // Verify isolation
    let settings_a: Option<UserSettings> = scoped_cache.get("tenant-a", "settings").await.unwrap();
    let settings_b: Option<UserSettings> = scoped_cache.get("tenant-b", "settings").await.unwrap();

    assert_eq!(settings_a.unwrap().theme, "dark");
    assert_eq!(settings_b.unwrap().theme, "light");

    // Invalidate tenant-a
    let invalidated = scoped_cache.invalidate_tenant("tenant-a").await.unwrap();
    assert_eq!(invalidated, 1);

    // Verify tenant-a is gone, tenant-b remains intact
    let settings_a: Option<UserSettings> = scoped_cache.get("tenant-a", "settings").await.unwrap();
    let settings_b: Option<UserSettings> = scoped_cache.get("tenant-b", "settings").await.unwrap();

    assert!(settings_a.is_none());
    assert_eq!(settings_b.unwrap().theme, "light");
}

#[tokio::test]
async fn tenant_scoped_cache_tag_isolation_and_quota_bounds() {
    let (_node, scoped_cache) = start_redis_tenant_cache().await;
    let scoped_cache = scoped_cache.with_max_keys_per_tenant(2);

    #[derive(Serialize, Deserialize, PartialEq, Debug)]
    struct Product {
        name: String,
    }

    // Tenant 1 sets item with tag "catalog"
    scoped_cache
        .set_with_tags(
            "t1",
            "p1",
            &Product {
                name: "T1 Item".into(),
            },
            &["catalog"],
            None,
        )
        .await
        .unwrap();

    // Tenant 2 sets item with identical tag name "catalog"
    scoped_cache
        .set_with_tags(
            "t2",
            "p1",
            &Product {
                name: "T2 Item".into(),
            },
            &["catalog"],
            None,
        )
        .await
        .unwrap();

    // Invalidate tag "catalog" for Tenant 1 ONLY
    let evicted = scoped_cache.invalidate_tag("t1", "catalog").await.unwrap();
    assert_eq!(evicted, 1);

    // Tenant 1's item is gone
    let t1_item: Option<Product> = scoped_cache.get("t1", "p1").await.unwrap();
    assert!(t1_item.is_none());

    // Tenant 2's item is PRESERVED!
    let t2_item: Option<Product> = scoped_cache.get("t2", "p1").await.unwrap();
    assert_eq!(t2_item.unwrap().name, "T2 Item");

    // Quota test: max 2 keys per tenant
    scoped_cache.set("t1", "k1", &"val1", None).await.unwrap();
    scoped_cache.set("t1", "k2", &"val2", None).await.unwrap();

    // 3rd key exceeds quota of 2
    let err = scoped_cache.set("t1", "k3", &"val3", None).await;
    assert!(matches!(
        err,
        Err(rume::cache::CacheError::QuotaExceeded(_))
    ));
}

#[tokio::test]
async fn test_tenant_cache_delimiter_isolation() {
    let (_node, scoped_cache) = start_redis_tenant_cache().await;

    // Tenant "a:b" with key "c"
    scoped_cache
        .set("a:b", "c", &"value_ab_c", None)
        .await
        .unwrap();

    // Tenant "a" with key "b:c"
    scoped_cache
        .set("a", "b:c", &"value_a_bc", None)
        .await
        .unwrap();

    // Verify no collision occurs due to length-prefixed formatting:
    // "tenant:3:a:b:c" vs "tenant:1:a:b:c"
    let val1: Option<String> = scoped_cache.get("a:b", "c").await.unwrap();
    let val2: Option<String> = scoped_cache.get("a", "b:c").await.unwrap();

    assert_eq!(val1.as_deref(), Some("value_ab_c"));
    assert_eq!(val2.as_deref(), Some("value_a_bc"));

    // Invalidation of tenant "a:b" must not touch tenant "a"
    scoped_cache.invalidate_tenant("a:b").await.unwrap();

    let val1_after: Option<String> = scoped_cache.get("a:b", "c").await.unwrap();
    let val2_after: Option<String> = scoped_cache.get("a", "b:c").await.unwrap();

    assert!(val1_after.is_none());
    assert_eq!(val2_after.as_deref(), Some("value_a_bc"));
}
