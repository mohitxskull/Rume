use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use rume::events::Event;
use rume::jobs::postgres::PostgresJobQueue;
use rume::jobs::{JobQueue, JobRegistry, JobWorker};
use rume::outbox::{OutboxMessage, OutboxRelay};
use rume::persistence::config::DatabaseConfig;
use rume::persistence::pool::DatabasePool;
use rume::persistence::postgres::PostgresBackend;
use rume::routing::health::{HealthCheck, HealthStatus};
use rume::tenant::model::Tenant;
use rume::tenant::resolver::HeaderTenantResolver;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use testcontainers_modules::testcontainers::ImageExt;
use tower::ServiceExt;

use workbench::app::build_app;
use workbench::jobs::{NotificationHandler, SendNotificationJob};
use workbench::migrations::workbench_migrator;
use workbench::models::{Issue, IssueCreatedEvent, Project};
use workbench::state::WorkbenchState;

#[tokio::test]
async fn test_workbench_full_lifecycle_multitenant_outbox_jobs_and_drain() {
    // =========================================================================
    // 1. Live PostgreSQL Container & Isolated Database Setup
    // =========================================================================
    let node = Postgres::default()
        .with_tag("16-alpine")
        .start()
        .await
        .expect("start postgres container");

    let host = node.get_host().await.expect("host");
    let port = node.get_host_port_ipv4(5432).await.expect("port");
    let db_name = format!("wb_test_{}", uuid::Uuid::new_v4().simple());

    let root_url = format!("postgres://postgres:postgres@{host}:{port}/postgres");
    let root_pool = sqlx::PgPool::connect(&root_url)
        .await
        .expect("connect root postgres");
    sqlx::query(&format!("CREATE DATABASE \"{db_name}\";"))
        .execute(&root_pool)
        .await
        .expect("create test db");

    let test_url = format!("postgres://postgres:postgres@{host}:{port}/{db_name}");
    let mut pg_config = DatabaseConfig::new(&test_url);
    pg_config.max_connections = 15;
    pg_config.auto_migrate = true;

    let backend = PostgresBackend::connect(&pg_config)
        .await
        .expect("connect test postgres backend");
    let pool = DatabasePool::new(backend, pg_config);

    // Run migrations
    let migrator = workbench_migrator();
    let migration_summary = pool
        .apply_auto_migrations(&migrator)
        .await
        .expect("apply auto migrations")
        .expect("migrations summary present");
    assert_eq!(
        migration_summary.applied.len(),
        2,
        "both core and workbench migrations must apply"
    );

    // Verify initial pool health
    let pool_hc = pool.health_check("primary_db");
    let pool_health = pool_hc.run().await;
    assert_eq!(pool_health.status, HealthStatus::Ok);

    // =========================================================================
    // 2. Job Queue, Handlers, and Background Worker Setup
    // =========================================================================
    let job_queue = Arc::new(PostgresJobQueue::new(pool.backend().pool().clone()));
    let notification_handler = NotificationHandler::new();
    let mut registry = JobRegistry::new();
    registry.register(notification_handler.clone());

    let worker = JobWorker::new(Arc::clone(&job_queue), Arc::new(registry));
    let worker_handle = worker.start();

    // =========================================================================
    // 3. Outbox Relay Setup (Dispatches IssueCreatedEvent -> SendNotificationJob)
    // =========================================================================
    let queue_for_relay = Arc::clone(&job_queue);
    let relay = OutboxRelay::new(Arc::new(pool.clone()), move |msg: &OutboxMessage| {
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
    });
    let _relay_handle = relay.start(Duration::from_millis(50));

    // Verify initial relay health
    let relay_hc = relay.health_check("outbox_relay");
    let relay_health = relay_hc.run().await;
    assert_eq!(relay_health.status, HealthStatus::Ok);

    // =========================================================================
    // 4. Multi-Tenant Application Instances Setup
    // =========================================================================
    let tenant_resolver = HeaderTenantResolver::new()
        .add_tenant(Tenant::new("acme-corp", "acme-corp"))
        .add_tenant(Tenant::new("globex", "globex"));

    let state1 = WorkbenchState {
        pool: pool.clone(),
        job_queue: Arc::clone(&job_queue),
        notification_handler: notification_handler.clone(),
        relay: Some(relay.clone()),
        worker: Some(worker.clone()),
    };
    let state2 = WorkbenchState {
        pool: pool.clone(),
        job_queue: Arc::clone(&job_queue),
        notification_handler: notification_handler.clone(),
        relay: Some(relay.clone()),
        worker: Some(worker.clone()),
    };

    let app1 = build_app(state1, tenant_resolver.clone());
    let app2 = build_app(state2, tenant_resolver.clone());

    // =========================================================================
    // 5. Security & Error Contract Tests (RFC 9457 & x-request-id)
    // =========================================================================

    // Case 5a: Unauthenticated request -> 401 Unauthorized
    {
        let req = Request::builder()
            .uri("/api/v1/projects")
            .method("GET")
            .header("x-tenant-id", "acme-corp")
            .body(Body::empty())
            .unwrap();
        let resp = app1.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        assert!(resp.headers().contains_key("x-request-id"));
        assert_eq!(
            resp.headers().get("content-type").unwrap(),
            "application/problem+json"
        );
    }

    // Case 5b: Missing tenant header -> 400 Bad Request
    {
        let req = Request::builder()
            .uri("/api/v1/projects")
            .method("GET")
            .header("authorization", "Bearer member-token")
            .body(Body::empty())
            .unwrap();
        let resp = app1.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        assert!(resp.headers().contains_key("x-request-id"));
        assert_eq!(
            resp.headers().get("content-type").unwrap(),
            "application/problem+json"
        );
    }

    // Case 5c: Viewer attempting write operation -> 403 Forbidden
    {
        let req = Request::builder()
            .uri("/api/v1/projects")
            .method("POST")
            .header("content-type", "application/json")
            .header("x-tenant-id", "acme-corp")
            .header("authorization", "Bearer viewer-token")
            .body(Body::from(r#"{"name":"Forbidden Project"}"#))
            .unwrap();
        let resp = app1.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        assert!(resp.headers().contains_key("x-request-id"));
        assert_eq!(
            resp.headers().get("content-type").unwrap(),
            "application/problem+json"
        );
    }

    // =========================================================================
    // 6. Multi-Tenant Project CRUD & Tenant Boundary Isolation
    // =========================================================================

    // Create Project for Acme Corp on Instance 1
    let acme_project_id: String;
    {
        let req = Request::builder()
            .uri("/api/v1/projects")
            .method("POST")
            .header("content-type", "application/json")
            .header("x-tenant-id", "acme-corp")
            .header("authorization", "Bearer member-token")
            .body(Body::from(
                r#"{"name":"Acme NextGen Core","description":"Primary Acme platform"}"#,
            ))
            .unwrap();
        let resp = app1.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        assert!(resp.headers().contains_key("x-request-id"));
        let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let project: Project = serde_json::from_slice(&body_bytes).unwrap();
        assert_eq!(project.tenant_id, "acme-corp");
        assert_eq!(project.name, "Acme NextGen Core");
        acme_project_id = project.id;
    }

    // Create Project for Globex on Instance 2
    let globex_project_id: String;
    {
        let req = Request::builder()
            .uri("/api/v1/projects")
            .method("POST")
            .header("content-type", "application/json")
            .header("x-tenant-id", "globex")
            .header("authorization", "Bearer admin-token")
            .body(Body::from(
                r#"{"name":"Globex Global Logistics","description":"Logistics ERP"}"#,
            ))
            .unwrap();
        let resp = app2.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let project: Project = serde_json::from_slice(&body_bytes).unwrap();
        assert_eq!(project.tenant_id, "globex");
        assert_eq!(project.name, "Globex Global Logistics");
        globex_project_id = project.id;
    }

    // Assert Acme cannot see Globex project in listing
    {
        let req = Request::builder()
            .uri("/api/v1/projects")
            .method("GET")
            .header("x-tenant-id", "acme-corp")
            .header("authorization", "Bearer member-token")
            .body(Body::empty())
            .unwrap();
        let resp = app2.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let projects: Vec<Project> = serde_json::from_slice(&body_bytes).unwrap();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].id, acme_project_id);
    }

    // Assert Acme cannot fetch Globex project by ID -> 404 Not Found
    {
        let req = Request::builder()
            .uri(format!("/api/v1/projects/{globex_project_id}"))
            .method("GET")
            .header("x-tenant-id", "acme-corp")
            .header("authorization", "Bearer member-token")
            .body(Body::empty())
            .unwrap();
        let resp = app1.clone().oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::NOT_FOUND,
            "Cross-tenant access must be rejected with 404"
        );
    }

    // =========================================================================
    // 7. Transactional Outbox & Background Processing Pipeline
    // =========================================================================
    let issue_id: String;
    {
        let req = Request::builder()
            .uri(format!("/api/v1/projects/{acme_project_id}/issues"))
            .method("POST")
            .header("content-type", "application/json")
            .header("x-tenant-id", "acme-corp")
            .header("authorization", "Bearer member-token")
            .body(Body::from(
                r#"{"title":"Fix Connection Pool Leak","description":"Resolve semaphore acquisition leak","priority":"urgent","assignee_id":"alice@acme.com"}"#,
            ))
            .unwrap();
        let resp = app1.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let issue: Issue = serde_json::from_slice(&body_bytes).unwrap();
        assert_eq!(issue.tenant_id, "acme-corp");
        assert_eq!(issue.title, "Fix Connection Pool Leak");
        assert_eq!(issue.status, "open");
        assert_eq!(issue.priority, "urgent");
        issue_id = issue.id;
    }

    // Wait for the background pipeline:
    // Outbox Relay -> SendNotificationJob -> NotificationHandler
    let start_wait = std::time::Instant::now();
    while notification_handler.deliveries.load(Ordering::SeqCst) < 1 {
        if start_wait.elapsed() > Duration::from_secs(10) {
            panic!("Timed out waiting for outbox relay and notification job handler to deliver message");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        notification_handler.deliveries.load(Ordering::SeqCst),
        1,
        "notification delivery count must reach 1"
    );

    // =========================================================================
    // 8. Update Issue & Verify Second Outbox Event Pipeline
    // =========================================================================
    {
        let req = Request::builder()
            .uri(format!("/api/v1/issues/{issue_id}"))
            .method("PATCH")
            .header("content-type", "application/json")
            .header("x-tenant-id", "acme-corp")
            .header("authorization", "Bearer member-token")
            .body(Body::from(
                r#"{"status":"resolved","priority":"medium"}"#,
            ))
            .unwrap();
        let resp = app2.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let issue: Issue = serde_json::from_slice(&body_bytes).unwrap();
        assert_eq!(issue.status, "resolved");
        assert_eq!(issue.priority, "medium");
    }

    // =========================================================================
    // 9. Concurrency & Graceful Shutdown Drain
    // =========================================================================
    // Drain background worker and relay gracefully
    let drained_worker = worker.drain(Duration::from_secs(5)).await;
    assert!(drained_worker, "worker must drain cleanly within timeout");
    assert!(worker_handle.is_finished());

    let drained_relay = relay.drain(Duration::from_secs(5)).await;
    assert!(drained_relay, "relay must drain cleanly within timeout");

    let drained_pool = pool.close_and_drain(Duration::from_secs(5)).await;
    assert!(drained_pool, "pool must close and drain connections cleanly");
    assert!(pool.status().is_closed);
}
