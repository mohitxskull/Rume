use axum::http::{Request, StatusCode};
use axum::response::IntoResponse;
use axum::routing::get;
use rume::cache::{Cache, CacheConfig, InMemoryCacheBackend};
use rume::prelude::*;
use rume::tenant::*;
use std::sync::Arc;

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
    let resolver = HeaderTenantResolver::new();

    // Tenant 1 has slug "acme" with id "t-100"
    resolver
        .try_add_tenant(Tenant::new("t-100", "acme"))
        .unwrap();

    // Cross-namespace collision 1: new tenant with id="acme" collides with existing slug="acme"
    let err1 = resolver.try_add_tenant(Tenant::new("acme", "globex"));
    assert!(matches!(err1, Err(rume::tenant::TenantError::Conflict(_))));

    // Cross-namespace collision 2: new tenant with slug="t-100" collides with existing id="t-100"
    let err2 = resolver.try_add_tenant(Tenant::new("t-200", "t-100"));
    assert!(matches!(err2, Err(rume::tenant::TenantError::Conflict(_))));

    // Subdomain resolver conflict check
    let sub_resolver = rume::tenant::SubdomainTenantResolver::new("example.com");
    sub_resolver
        .try_add_tenant(Tenant::new("t-1", "acme"))
        .unwrap();
    let sub_err = sub_resolver.try_add_tenant(Tenant::new("t-2", "acme"));
    assert!(matches!(
        sub_err,
        Err(rume::tenant::TenantError::Conflict(_))
    ));
}

#[tokio::test]
async fn test_header_tenant_resolver_slug_conflict_rejection() {
    let resolver = HeaderTenantResolver::new();
    let res1 = resolver.try_add_tenant(Tenant::new("t-1", "acme"));
    assert!(res1.is_ok());

    // Duplicate slug under different ID must return Conflict error
    let res2 = resolver.try_add_tenant(Tenant::new("t-2", "acme"));
    assert!(matches!(res2, Err(TenantError::Conflict(_))));

    // Adding same tenant (same ID and slug) is idempotent
    let res3 = resolver.try_add_tenant(Tenant::new("t-1", "acme"));
    assert!(res3.is_ok());
}

#[tokio::test]
async fn test_subdomain_tenant_resolver_case_insensitivity() {
    let resolver =
        SubdomainTenantResolver::new("platform.io").add_tenant(Tenant::new("t-1", "acme"));

    let app = Router::new()
        .route(
            "/tenant",
            get(|TenantContext(tenant): TenantContext| async move {
                Json(serde_json::json!({ "id": tenant.id, "slug": tenant.slug }))
            }),
        )
        .layer(TenantLayer::required(resolver));

    let test_app = TestApp::new(app);

    let req = Request::builder()
        .uri("/tenant")
        .header("host", "ACME.platform.io:8080")
        .body(axum::body::Body::empty())
        .unwrap();

    let res = test_app.request(req).await;
    assert_eq!(res.status(), StatusCode::OK);
    let body: serde_json::Value = serde_json::from_str(&TestApp::body_text(res).await).unwrap();
    assert_eq!(body["id"], "t-100".replace("100", "1"));
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

fn start_tenant_cache() -> TenantScopedCache<InMemoryCacheBackend> {
    let backend = InMemoryCacheBackend::new();
    let base_cache = Cache::new(backend, CacheConfig::new("memory://test"));
    let quota_store = Arc::new(InMemoryTenantQuotaStore::new());
    TenantScopedCache::new(base_cache).with_quota_store(quota_store)
}

#[tokio::test]
async fn test_tenant_scoped_cache() {
    let scoped_cache = start_tenant_cache();

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
    let scoped_cache = start_tenant_cache();
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
    let scoped_cache = start_tenant_cache();

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

#[tokio::test]
async fn test_tenant_cache_quota_idempotent_key_updates() {
    let scoped_cache = start_tenant_cache().with_max_keys_per_tenant(2);

    // Repeated updates to the SAME key k1 do not consume additional quota
    for i in 0..10 {
        scoped_cache
            .set("tenant_quota_test", "k1", &format!("val_{i}"), None)
            .await
            .expect("key update must succeed");
    }

    // Tenant can still write a distinct second key k2 (within quota of 2)
    scoped_cache
        .set("tenant_quota_test", "k2", &"val_k2", None)
        .await
        .expect("second distinct key within quota must succeed");

    // Attempting a third distinct key k3 exceeds quota of 2
    let err = scoped_cache
        .set("tenant_quota_test", "k3", &"val_k3", None)
        .await;
    assert!(matches!(
        err,
        Err(rume::cache::CacheError::QuotaExceeded(_))
    ));

    // Updating existing key k2 still succeeds!
    scoped_cache
        .set("tenant_quota_test", "k2", &"val_k2_updated", None)
        .await
        .expect("updating existing key k2 must succeed even when quota is full");
}

#[tokio::test]
async fn test_in_memory_tenant_quota_store_lease_churn_never_bypasses_limit() {
    use rume::tenant::quota::{InMemoryTenantQuotaStore, TenantQuotaStore};

    let store = InMemoryTenantQuotaStore::new();
    let tenant_id = "tenant_churn_test";
    let max_leases = 2;

    // Acquire up to limit
    assert!(store.acquire(tenant_id, max_leases).await.unwrap());
    assert!(store.acquire(tenant_id, max_leases).await.unwrap());
    // Third acquisition must fail
    assert!(!store.acquire(tenant_id, max_leases).await.unwrap());

    // Release 1 lease and acquire again in rapid churn cycles
    for _ in 0..10 {
        store.release(tenant_id).await.unwrap();
        // One slot opened up
        assert!(store.acquire(tenant_id, max_leases).await.unwrap());
        // Capacity is full again; exceeding limit MUST fail
        assert!(!store.acquire(tenant_id, max_leases).await.unwrap());
    }

    // Both leases released
    store.release(tenant_id).await.unwrap();
    store.release(tenant_id).await.unwrap();
    // Releasing beyond 0 saturates cleanly at 0
    store.release(tenant_id).await.unwrap();

    // Now 2 fresh acquisitions succeed
    assert!(store.acquire(tenant_id, max_leases).await.unwrap());
    assert!(store.acquire(tenant_id, max_leases).await.unwrap());
    // And 3rd still fails
    assert!(!store.acquire(tenant_id, max_leases).await.unwrap());
}

#[tokio::test]
async fn test_in_memory_tenant_quota_ttl_expiration() {
    use rume::tenant::quota::{InMemoryTenantQuotaStore, TenantQuotaStore};
    use std::time::Duration;

    let store = InMemoryTenantQuotaStore::new();
    let tenant_id = "tenant_ttl_test";
    let max_keys = 2;

    // 1. Record k1 with 30ms TTL
    assert!(
        store
            .record_key(tenant_id, "k1", Some(Duration::from_millis(30)), max_keys)
            .await
            .unwrap()
    );

    // 2. Record k2 with 2000ms TTL
    assert!(
        store
            .record_key(tenant_id, "k2", Some(Duration::from_secs(2)), max_keys)
            .await
            .unwrap()
    );

    assert_eq!(store.count(tenant_id).await.unwrap(), 2);

    // 3. While both active, k3 is rejected because max_keys (2) is reached
    assert!(
        !store
            .record_key(tenant_id, "k3", None, max_keys)
            .await
            .unwrap()
    );

    // 4. Wait for k1 to expire
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Active count must now reflect 1 (expired k1 is pruned/excluded)
    assert_eq!(store.count(tenant_id).await.unwrap(), 1);

    // 5. k3 can now be recorded successfully
    assert!(
        store
            .record_key(tenant_id, "k3", None, max_keys)
            .await
            .unwrap()
    );
    assert_eq!(store.count(tenant_id).await.unwrap(), 2);

    // 6. Refreshing k2 with new TTL succeeds even at max_keys
    assert!(
        store
            .record_key(tenant_id, "k2", Some(Duration::from_secs(5)), max_keys)
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn test_concurrent_same_key_quota_updates() {
    use rume::tenant::quota::{InMemoryTenantQuotaStore, TenantQuotaStore};
    use std::time::Duration;

    let store = Arc::new(InMemoryTenantQuotaStore::new());
    let tenant_id = "tenant_concurrent_test";
    let max_keys = 1;

    // Spawn 10 concurrent tasks all updating the SAME key
    let mut handles = Vec::new();
    for i in 0..10 {
        let store = Arc::clone(&store);
        handles.push(tokio::spawn(async move {
            store
                .record_key(
                    tenant_id,
                    "shared_key",
                    Some(Duration::from_secs(10 + i)),
                    max_keys,
                )
                .await
                .unwrap()
        }));
    }

    for handle in handles {
        let ok = handle.await.unwrap();
        assert!(ok, "all concurrent updates to the same key must succeed");
    }

    // Must still count as exactly 1 key, respecting max_keys = 1
    assert_eq!(store.count(tenant_id).await.unwrap(), 1);
}
