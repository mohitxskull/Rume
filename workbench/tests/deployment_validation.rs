use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use rume::events::Event;
use rume::jobs::postgres::PostgresJobQueue;
use rume::jobs::redis::RedisIdempotencyStore;
use rume::jobs::{JobQueue, JobRegistry, JobWorker};
use rume::outbox::{OutboxMessage, OutboxRelay};
use rume::persistence::config::DatabaseConfig;
use rume::persistence::pool::DatabasePool;
use rume::persistence::postgres::PostgresBackend;
use rume::tenant::model::Tenant;
use rume::tenant::resolver::HeaderTenantResolver;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::redis::Redis;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use testcontainers_modules::testcontainers::ImageExt;
use tower::ServiceExt;

use workbench::app::build_app;
use workbench::jobs::{NotificationHandler, SendNotificationJob};
use workbench::migrations::workbench_migrator;
use workbench::models::{IssueCreatedEvent, Project};
use workbench::state::WorkbenchState;

#[tokio::test]
async fn test_workbench_deployment_validation_multi_instance_concurrent_tenants_and_redis() {
    // =========================================================================
    // 1. Start Real PostgreSQL and Redis Containers
    // =========================================================================
    let pg_node = Postgres::default()
        .with_tag("16-alpine")
        .start()
        .await
        .expect("start postgres container");

    let pg_host = pg_node.get_host().await.expect("host");
    let pg_port = pg_node.get_host_port_ipv4(5432).await.expect("port");
    let db_name = format!("deploy_test_{}", uuid::Uuid::new_v4().simple());

    let root_url = format!("postgres://postgres:postgres@{pg_host}:{pg_port}/postgres");
    let root_pool = sqlx::PgPool::connect(&root_url)
        .await
        .expect("connect root postgres");
    sqlx::query(&format!("CREATE DATABASE \"{db_name}\";"))
        .execute(&root_pool)
        .await
        .expect("create test db");

    let redis_node = Redis::default()
        .with_tag("7-alpine")
        .start()
        .await
        .expect("start redis container");
    let redis_host = redis_node.get_host().await.expect("redis host");
    let redis_port = redis_node.get_host_port_ipv4(6379).await.expect("redis port");
    let redis_url = format!("redis://{redis_host}:{redis_port}");

    let test_url = format!("postgres://postgres:postgres@{pg_host}:{pg_port}/{db_name}");
    let mut pg_config = DatabaseConfig::new(&test_url);
    pg_config.max_connections = 25;
    pg_config.auto_migrate = true;

    let backend = PostgresBackend::connect(&pg_config)
        .await
        .expect("connect postgres");
    let pool = DatabasePool::new(backend, pg_config.clone());

    // Run migrations
    let migrator = workbench_migrator();
    let migration_summary = pool
        .apply_auto_migrations(&migrator)
        .await
        .expect("apply auto migrations")
        .expect("migration summary");
    assert_eq!(migration_summary.applied.len(), 2);

    // =========================================================================
    // 2. Redis Idempotency Store & Background Jobs Setup
    // =========================================================================
    let redis_cfg = deadpool_redis::Config::from_url(&redis_url);
    let redis_pool = redis_cfg
        .create_pool(Some(deadpool_redis::Runtime::Tokio1))
        .expect("create redis pool");
    let idempotency_store = Arc::new(RedisIdempotencyStore::new(redis_pool));

    let job_queue = Arc::new(PostgresJobQueue::new(pool.backend().pool().clone()));
    let notification_handler = NotificationHandler::new();
    let mut registry = JobRegistry::new();
    registry.register(notification_handler.clone());
    let shared_registry = Arc::new(registry);

    // Multiple background workers sharing the job queue and redis idempotency store
    let worker1 = JobWorker::new(Arc::clone(&job_queue), Arc::clone(&shared_registry))
        .with_worker_id("worker-inst-1")
        .with_idempotency_store(Arc::clone(&idempotency_store) as Arc<dyn rume::jobs::IdempotencyStore>);
    let worker2 = JobWorker::new(Arc::clone(&job_queue), Arc::clone(&shared_registry))
        .with_worker_id("worker-inst-2")
        .with_idempotency_store(Arc::clone(&idempotency_store) as Arc<dyn rume::jobs::IdempotencyStore>);

    let worker1_handle = worker1.start();
    let worker2_handle = worker2.start();

    // =========================================================================
    // 3. Multiple Outbox Relays Competing on SKIP LOCKED
    // =========================================================================
    let queue_for_relay = Arc::clone(&job_queue);
    let relay_dispatch = move |msg: &OutboxMessage| {
        let queue = Arc::clone(&queue_for_relay);
        let msg = msg.clone();
        async move {
            if msg.event_type == IssueCreatedEvent::TYPE {
                let event: IssueCreatedEvent = serde_json::from_slice(&msg.payload)
                    .map_err(|e| rume::outbox::OutboxError::Repository(e.to_string()))?;
                let job = SendNotificationJob {
                    tenant_id: event.tenant_id.clone(),
                    recipient: event
                        .assignee_id
                        .unwrap_or_else(|| "unassigned@workbench.local".to_string()),
                    subject: format!("Issue Created: {}", event.title),
                    body: format!(
                        "Issue {} created in project {}",
                        event.issue_id, event.project_id
                    ),
                    idempotency_key: format!("notify-issue-{}", event.issue_id),
                };
                queue
                    .enqueue(job)
                    .await
                    .map_err(|e| rume::outbox::OutboxError::Dispatch(e.to_string()))?;
            }
            Ok(())
        }
    };

    let relay1 = OutboxRelay::new(Arc::new(pool.clone()), relay_dispatch.clone());
    let relay2 = OutboxRelay::new(Arc::new(pool.clone()), relay_dispatch);

    let _relay1_handle = relay1.start(Duration::from_millis(50));
    let _relay2_handle = relay2.start(Duration::from_millis(50));

    // =========================================================================
    // 4. Two Application Instances with Multi-Tenant Routing
    // =========================================================================
    let tenant_resolver = HeaderTenantResolver::new()
        .add_tenant(Tenant::new("acme-corp", "acme-corp"))
        .add_tenant(Tenant::new("globex", "globex"));

    let state1 = WorkbenchState {
        pool: pool.clone(),
        job_queue: Arc::clone(&job_queue),
        notification_handler: notification_handler.clone(),
        relay: Some(relay1.clone()),
        worker: Some(worker1.clone()),
    };
    let state2 = WorkbenchState {
        pool: pool.clone(),
        job_queue: Arc::clone(&job_queue),
        notification_handler: notification_handler.clone(),
        relay: Some(relay2.clone()),
        worker: Some(worker2.clone()),
    };

    let app1 = build_app(state1, tenant_resolver.clone());
    let app2 = build_app(state2, tenant_resolver.clone());

    // Setup project for Acme on App 1
    let acme_proj_id: String;
    {
        let req = Request::builder()
            .uri("/api/v1/projects")
            .method("POST")
            .header("content-type", "application/json")
            .header("x-tenant-id", "acme-corp")
            .header("authorization", "Bearer member-token")
            .body(Body::from(r#"{"name":"Acme Infra"}"#))
            .unwrap();
        let resp = app1.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let proj: Project = serde_json::from_slice(&bytes).unwrap();
        acme_proj_id = proj.id;
    }

    // Setup project for Globex on App 2
    let globex_proj_id: String;
    {
        let req = Request::builder()
            .uri("/api/v1/projects")
            .method("POST")
            .header("content-type", "application/json")
            .header("x-tenant-id", "globex")
            .header("authorization", "Bearer admin-token")
            .body(Body::from(r#"{"name":"Globex Supply"}"#))
            .unwrap();
        let resp = app2.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let proj: Project = serde_json::from_slice(&bytes).unwrap();
        globex_proj_id = proj.id;
    }

    // =========================================================================
    // 5. Concurrent Issue Creation Across Tenants and Instances
    // =========================================================================
    let total_issues = 12;
    let mut handles = Vec::new();

    for i in 0..total_issues {
        let app = if i % 2 == 0 {
            app1.clone()
        } else {
            app2.clone()
        };
        let (tenant, proj_id) = if i % 2 == 0 {
            ("acme-corp", acme_proj_id.clone())
        } else {
            ("globex", globex_proj_id.clone())
        };

        handles.push(tokio::spawn(async move {
            let req = Request::builder()
                .uri(format!("/api/v1/projects/{proj_id}/issues"))
                .method("POST")
                .header("content-type", "application/json")
                .header("x-tenant-id", tenant)
                .header("authorization", "Bearer member-token")
                .body(Body::from(format!(
                    r#"{{"title":"Concurrent Issue #{i}","priority":"high"}}"#
                )))
                .unwrap();
            let resp = app.oneshot(req).await.unwrap();
            assert_eq!(resp.status(), StatusCode::CREATED);
        }));
    }

    for h in handles {
        h.await.unwrap();
    }

    // =========================================================================
    // 6. Assert Full Outbox & Deduplicated Worker Processing
    // =========================================================================
    let start_wait = std::time::Instant::now();
    while notification_handler.deliveries.load(Ordering::SeqCst) < total_issues {
        if start_wait.elapsed() > Duration::from_secs(15) {
            panic!(
                "Timed out waiting for all concurrent notifications to be delivered (delivered: {})",
                notification_handler.deliveries.load(Ordering::SeqCst)
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    assert_eq!(
        notification_handler.deliveries.load(Ordering::SeqCst),
        total_issues,
        "Every created issue must result in exactly one processed notification"
    );

    // =========================================================================
    // 7. Graceful Termination of All Workers, Relays, and Pools
    // =========================================================================
    let d_w1 = worker1.drain(Duration::from_secs(5)).await;
    let d_w2 = worker2.drain(Duration::from_secs(5)).await;
    assert!(d_w1 && d_w2);
    assert!(worker1_handle.is_finished() && worker2_handle.is_finished());

    let d_r1 = relay1.drain(Duration::from_secs(5)).await;
    let d_r2 = relay2.drain(Duration::from_secs(5)).await;
    assert!(d_r1 && d_r2);

    let d_pool = pool.close_and_drain(Duration::from_secs(5)).await;
    assert!(d_pool);
    assert!(pool.status().is_closed);
}
