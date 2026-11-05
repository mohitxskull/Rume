//! Comprehensive failure-oriented enterprise integration pipeline test suite.
//!
//! Verifies the end-to-end integration of:
//! Authentication -> Multi-Tenant Resolution -> Policy Authorization ->
//! Atomic Database Transaction + Outbox Enlistment -> Relay Event Dispatch ->
//! Background Worker Execution -> Tracing & Metrics -> Failure Recovery & Graceful Drain.

#![cfg(feature = "postgres")]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::{Json, State};
use axum::http::{Request, StatusCode};
use axum::response::IntoResponse;
use axum::routing::post;
use serde::{Deserialize, Serialize};
use tower::ServiceExt;

use rume::app::observability::ObservabilityLayer;
#[cfg(feature = "sqlite")]
use rume::app::state::{Availability, AvailabilityState};
use rume::auth::identity::{AuthUser, Authenticated};
use rume::auth::layer::{AuthLayer, PolicyLayer};
use rume::auth::policy::RequirePermission;
#[cfg(feature = "sqlite")]
use rume::auth::policy::authorize;
use rume::auth::schemes::BearerTokenScheme;
use rume::coordination::LeaderElector;
use rume::coordination::postgres::PostgresLeaderElector;
use rume::error::AppError;
use rume::events::Event;
use rume::jobs::{Job, JobContext, JobError, JobQueue, JobRegistry, JobWorker, PostgresJobQueue};
use rume::metrics::MetricsRegistry;
use rume::outbox::OutboxStatus;
use rume::outbox::{
    OutboxError, OutboxMessage, OutboxRelay, OutboxRelayConfig, OutboxRepository,
    TransactionalOutbox,
};
#[cfg(feature = "sqlite")]
use rume::persistence::SqliteBackend;
use rume::persistence::postgres::PostgresBackend;
use rume::persistence::{
    DatabaseBackend, DatabaseConfig, DatabasePool, DatabaseTransaction, PersistenceError,
};
use rume::routing::HealthCheck;
use rume::tenant::layer::TenantLayer;
use rume::tenant::resolver::HeaderTenantResolver;
use rume::tenant::{Tenant, TenantContext};

use crate::common;

#[cfg(feature = "sqlite")]
#[derive(Debug, Clone, Serialize, Deserialize)]
struct OrderBilledEvent {
    pub tenant_id: String,
    pub order_id: String,
    pub amount_cents: u64,
}

#[cfg(feature = "sqlite")]
impl Event for OrderBilledEvent {
    const TYPE: &'static str = "billing.order_billed";
}

#[cfg(feature = "sqlite")]
#[derive(Debug, Clone, Serialize, Deserialize)]
struct FulfillOrderJob {
    pub tenant_id: String,
    pub order_id: String,
}

#[cfg(feature = "sqlite")]
impl Job for FulfillOrderJob {
    const NAME: &'static str = "fulfillment.fulfill_order";
}

#[cfg(feature = "sqlite")]
struct FulfillOrderHandler {
    jobs_processed: Arc<AtomicUsize>,
    metrics: Arc<MetricsRegistry>,
    notify: Arc<tokio::sync::Notify>,
}

#[cfg(feature = "sqlite")]
impl rume::jobs::JobHandler<FulfillOrderJob> for FulfillOrderHandler {
    fn handle<'a>(
        &'a self,
        _ctx: &'a JobContext,
        job: FulfillOrderJob,
    ) -> rume::jobs::BoxFuture<'a, Result<(), JobError>> {
        let jp = Arc::clone(&self.jobs_processed);
        let m = Arc::clone(&self.metrics);
        let notify = Arc::clone(&self.notify);
        Box::pin(async move {
            assert_eq!(job.tenant_id, "acme-corp");
            assert_eq!(job.order_id, "ord_9901");
            jp.fetch_add(1, Ordering::SeqCst);
            m.record_http_request("POST", 201, Duration::from_millis(15));
            notify.notify_one();
            Ok(())
        })
    }
}

#[tokio::test]
#[cfg(feature = "sqlite")]
async fn realistic_enterprise_pipeline_end_to_end() {
    // 1. Setup Framework Subsystems
    let db = Arc::new(SqliteBackend::in_memory().await.unwrap());
    db.init_outbox_table().await.unwrap();
    db.execute(
        "CREATE TABLE orders (id TEXT PRIMARY KEY, tenant_id TEXT NOT NULL, status TEXT NOT NULL);",
    )
    .await
    .unwrap();

    let pool = DatabasePool::new((*db).clone(), DatabaseConfig::default());

    let (_db_name, pg_config) = common::containers::create_isolated_postgres_db().await;
    let pg_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(&pg_config.url)
        .await
        .expect("pg pool");
    let queue = Arc::new(PostgresJobQueue::new(pg_pool));
    queue.init_schema().await.expect("init jobs schema");

    let metrics = Arc::new(MetricsRegistry::new());

    // 2. Authentication & Tenant Resolution
    let tenant = Tenant::new("acme-corp", "acme");
    let tenant_ctx = TenantContext(tenant);
    assert_eq!(tenant_ctx.id, "acme-corp");

    // 3. Policy Authorization
    let user = AuthUser::new("user_finance_1")
        .with_role("finance_admin")
        .with_permission("billing:charge");
    assert!(
        authorize(&user, &(), RequirePermission("billing:charge")).is_ok(),
        "finance_admin must be authorized to charge"
    );

    // 4. Atomic Database Transaction + Outbox Enlistment
    let order_id = "ord_9901";
    let tenant_id = tenant_ctx.id.clone();
    let event = OrderBilledEvent {
        tenant_id: tenant_id.clone(),
        order_id: order_id.to_string(),
        amount_cents: 15000,
    };
    let outbox_msg = OutboxMessage::from_event(&event).unwrap();

    // Execute atomic transaction: update business balance + stage outbox message
    let tx_res = pool
        .transaction(|tx| {
            let db = Arc::clone(&db);
            let tenant_key = tenant_id.clone();
            async move {
                tx.execute(&format!(
                    "INSERT INTO orders (id, tenant_id, status) VALUES ('{order_id}', '{tenant_key}', 'paid');"
                ))
                .await?;
                db.save_tx(&tx, outbox_msg).await.unwrap();
                Ok::<(), PersistenceError>(())
            }
        })
        .await;
    assert!(tx_res.is_ok(), "database transaction must commit cleanly");

    // Verify database state
    let state_val = pool
        .backend()
        .query_scalar(&format!(
            "SELECT status FROM orders WHERE id = '{order_id}';"
        ))
        .await
        .unwrap();
    assert_eq!(state_val.as_deref(), Some("paid"));

    // 5. Durable Outbox Relay Dispatch
    let event_published = Arc::new(AtomicBool::new(false));
    let q_for_relay = Arc::clone(&queue);
    let ep = Arc::clone(&event_published);

    let relay = OutboxRelay::new(Arc::clone(&db), move |msg| {
        let q = Arc::clone(&q_for_relay);
        let ep = Arc::clone(&ep);
        let billed: OrderBilledEvent = msg.deserialize_payload().unwrap();
        async move {
            ep.store(true, Ordering::SeqCst);
            // Result of published domain event: enqueue fulfillment background job
            let job = FulfillOrderJob {
                tenant_id: billed.tenant_id,
                order_id: billed.order_id,
            };
            q.enqueue(job).await.unwrap();
            Ok(())
        }
    });

    let published_count = relay.poll_and_dispatch(10).await.unwrap();
    assert_eq!(published_count, 1);
    assert!(event_published.load(Ordering::SeqCst));
    assert_eq!(db.pending_count().await.unwrap(), 0);

    // 6. Background Worker Execution & Metrics Recording
    let mut registry = JobRegistry::new();
    let jobs_processed = Arc::new(AtomicUsize::new(0));
    let notify = Arc::new(tokio::sync::Notify::new());

    registry.register(FulfillOrderHandler {
        jobs_processed: Arc::clone(&jobs_processed),
        metrics: Arc::clone(&metrics),
        notify: Arc::clone(&notify),
    });

    let worker = JobWorker::new(Arc::clone(&queue), Arc::new(registry));
    worker.start();

    // Deterministically await background job execution
    notify.notified().await;

    let drained = worker.drain(Duration::from_secs(2)).await;
    assert!(
        drained,
        "worker must successfully process queued job and drain"
    );
    assert_eq!(jobs_processed.load(Ordering::SeqCst), 1);
    assert_eq!(queue.pending_count().await.unwrap(), 0);

    // Verify metrics recorded
    let body = metrics.render_prometheus();
    assert!(body.contains("http_requests_total"));
    assert!(body.contains("POST"));
}

#[tokio::test]
#[cfg(feature = "sqlite")]
async fn pipeline_failure_recovery_and_atomicity_invariants() {
    let db = Arc::new(SqliteBackend::in_memory().await.unwrap());
    db.init_outbox_table().await.unwrap();
    db.execute(
        "CREATE TABLE orders (id TEXT PRIMARY KEY, tenant_id TEXT NOT NULL, status TEXT NOT NULL);",
    )
    .await
    .unwrap();

    let pool = DatabasePool::new((*db).clone(), DatabaseConfig::default());
    let availability = Availability::new();

    // Invariant 1: Transaction Failure Aborts Business State AND Outbox Message Atomically
    let failed_event = OrderBilledEvent {
        tenant_id: "tenant-fail".into(),
        order_id: "ord_fail".into(),
        amount_cents: 100,
    };
    let failed_msg = OutboxMessage::from_event(&failed_event).unwrap();

    let tx_fail = pool
        .transaction(|tx| {
            let db = Arc::clone(&db);
            async move {
                tx.execute(
                    "INSERT INTO orders (id, tenant_id, status) VALUES ('ord_fail', 'tenant-fail', 'corrupted');",
                )
                .await?;
                db.save_tx(&tx, failed_msg).await.unwrap();
                Err::<(), PersistenceError>(PersistenceError::QueryFailed(
                    "constraint failed".into(),
                ))
            }
        })
        .await;

    assert!(tx_fail.is_err());
    // Zero mutations persisted
    assert_eq!(
        pool.backend()
            .query_scalar("SELECT status FROM orders WHERE id = 'ord_fail';")
            .await
            .unwrap(),
        None
    );
    assert_eq!(db.pending_count().await.unwrap(), 0);

    // Invariant 2: Expired Lease Recovery after Worker Crash
    let valid_event = OrderBilledEvent {
        tenant_id: "tenant-crash".into(),
        order_id: "ord_recovered".into(),
        amount_cents: 5000,
    };
    let valid_msg = OutboxMessage::from_event(&valid_event).unwrap();
    db.save(valid_msg).await.unwrap();

    // Worker 1 claims message with 0-second lease (simulating immediate crash/expiry)
    let claimed = db.claim_pending_with_lease(1, 0).await.unwrap();
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].status, OutboxStatus::InFlight);

    // Immediate secondary claim returns 0
    let claimed_none = db.claim_pending_with_lease(1, 10).await.unwrap();
    assert_eq!(claimed_none.len(), 0);

    // Worker recovery loop recovers abandoned claim
    let recovered = db.recover_expired_claims(0).await.unwrap();
    assert_eq!(recovered, 1, "abandoned outbox claim must be recovered");

    // Second relay now successfully processes the recovered event
    let relay_success = Arc::new(AtomicBool::new(false));
    let rs = Arc::clone(&relay_success);
    let relay = OutboxRelay::new(Arc::clone(&db), move |_msg| {
        let rs = Arc::clone(&rs);
        async move {
            rs.store(true, Ordering::SeqCst);
            Ok(())
        }
    });

    let count = relay.poll_and_dispatch(5).await.unwrap();
    assert_eq!(count, 1);
    assert!(relay_success.load(Ordering::SeqCst));

    // Invariant 3: Graceful Drain and Availability State Transition
    availability.set(AvailabilityState::RefusingTraffic);
    assert_eq!(availability.state(), AvailabilityState::RefusingTraffic);

    let drained = pool.close_and_drain(Duration::from_millis(50)).await;
    assert!(drained, "pool must cleanly drain during shutdown");
}

#[derive(Clone)]
struct UnifiedAppState {
    pool: Arc<DatabasePool<PostgresBackend>>,
}

#[derive(Debug, Deserialize, Serialize)]
struct CreateEnterpriseOrderRequest {
    order_id: String,
    amount_cents: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct EnterpriseOrderCreatedEvent {
    tenant_id: String,
    order_id: String,
    amount_cents: i64,
}

impl Event for EnterpriseOrderCreatedEvent {
    const TYPE: &'static str = "enterprise.order_created";
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FulfillEnterpriseOrderJob {
    tenant_id: String,
    order_id: String,
}

impl Job for FulfillEnterpriseOrderJob {
    const NAME: &'static str = "enterprise.fulfill_order";
}

async fn enterprise_create_order_endpoint(
    TenantContext(tenant): TenantContext,
    Authenticated(user): Authenticated<AuthUser>,
    State(state): State<UnifiedAppState>,
    Json(payload): Json<CreateEnterpriseOrderRequest>,
) -> Result<impl IntoResponse, AppError> {
    let event = EnterpriseOrderCreatedEvent {
        tenant_id: tenant.id.clone(),
        order_id: payload.order_id.clone(),
        amount_cents: payload.amount_cents,
    };
    let outbox_msg =
        OutboxMessage::from_event(&event).map_err(|e| AppError::internal(e.to_string()))?;

    let pool_for_tx = Arc::clone(&state.pool);
    let order_id = payload.order_id.clone();
    let tenant_id = tenant.id.clone();
    let amount = payload.amount_cents;

    state
        .pool
        .transaction(move |tx| {
            let pool = Arc::clone(&pool_for_tx);
            let order_id = order_id.clone();
            let tenant_id = tenant_id.clone();
            let outbox_msg = outbox_msg.clone();
            async move {
                let sql = format!(
                    "INSERT INTO enterprise_orders (id, tenant_id, amount_cents, status) VALUES ('{}', '{}', {}, 'created');",
                    order_id, tenant_id, amount
                );
                tx.execute(&sql).await?;
                pool.save_tx(&tx, outbox_msg)
                    .await
                    .map_err(|e| PersistenceError::QueryFailed(e.to_string()))?;
                Ok::<(), PersistenceError>(())
            }
        })
        .await
        .map_err(|e| AppError::internal(e.to_string()))?;

    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({
            "order_id": payload.order_id,
            "status": "created",
            "created_by": user.id,
        })),
    ))
}

struct IdempotentFulfillmentHandler {
    backend: Arc<PostgresBackend>,
    executions: Arc<AtomicUsize>,
    notify: Arc<tokio::sync::Notify>,
}

impl rume::jobs::JobHandler<FulfillEnterpriseOrderJob> for IdempotentFulfillmentHandler {
    fn handle<'a>(
        &'a self,
        _ctx: &'a JobContext,
        job: FulfillEnterpriseOrderJob,
    ) -> rume::jobs::BoxFuture<'a, Result<(), JobError>> {
        let backend = Arc::clone(&self.backend);
        let executions = Arc::clone(&self.executions);
        let notify = Arc::clone(&self.notify);
        Box::pin(async move {
            let sql = format!(
                "INSERT INTO enterprise_fulfillments (order_id, tenant_id, dispatch_count) VALUES ('{}', '{}', 1) \
                 ON CONFLICT (order_id) DO UPDATE SET dispatch_count = enterprise_fulfillments.dispatch_count + 1;",
                job.order_id, job.tenant_id
            );
            backend
                .execute(&sql)
                .await
                .map_err(|e| JobError::Retryable(e.to_string()))?;
            executions.fetch_add(1, Ordering::SeqCst);
            notify.notify_one();
            Ok(())
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SlowDrainJob {
    job_id: String,
}

impl Job for SlowDrainJob {
    const NAME: &'static str = "enterprise.slow_drain";
}

struct SlowDrainHandler {
    completed: Arc<AtomicBool>,
    started: Arc<tokio::sync::Notify>,
}

impl rume::jobs::JobHandler<SlowDrainJob> for SlowDrainHandler {
    fn handle<'a>(
        &'a self,
        _ctx: &'a JobContext,
        _job: SlowDrainJob,
    ) -> rume::jobs::BoxFuture<'a, Result<(), JobError>> {
        let completed = Arc::clone(&self.completed);
        let started = Arc::clone(&self.started);
        Box::pin(async move {
            started.notify_one();
            tokio::time::sleep(Duration::from_millis(250)).await;
            completed.store(true, Ordering::SeqCst);
            Ok(())
        })
    }
}

#[tokio::test]
async fn test_postgres_unified_enterprise_failure_pipeline() {
    // =========================================================================
    // 1. Single PostgreSQL Database Setup
    // =========================================================================
    let (_db_name, pg_config) = common::containers::create_isolated_postgres_db().await;
    let pg_backend = Arc::new(
        PostgresBackend::connect(&pg_config)
            .await
            .expect("connect to unified postgres instance"),
    );

    // Initialize transactional outbox table
    pg_backend
        .init_outbox_table()
        .await
        .expect("init outbox table");

    // Initialize business domain tables
    pg_backend
        .execute(
            "CREATE TABLE enterprise_orders (
                id TEXT PRIMARY KEY,
                tenant_id TEXT NOT NULL,
                amount_cents BIGINT NOT NULL,
                status TEXT NOT NULL
            );",
        )
        .await
        .expect("create enterprise_orders table");

    pg_backend
        .execute(
            "CREATE TABLE enterprise_fulfillments (
                order_id TEXT PRIMARY KEY,
                tenant_id TEXT NOT NULL,
                dispatch_count INT NOT NULL DEFAULT 1
            );",
        )
        .await
        .expect("create enterprise_fulfillments table");

    // Initialize PostgreSQL job queue sharing the EXACT same database pool
    let pg_pool = pg_backend.pool().clone();
    let queue =
        Arc::new(PostgresJobQueue::new(pg_pool).with_lease_duration(Duration::from_millis(1500)));
    queue.init_schema().await.expect("init jobs schema");

    let pool = Arc::new(DatabasePool::new(
        (*pg_backend).clone(),
        DatabaseConfig::default(),
    ));

    // =========================================================================
    // 2. Axum Application Shell with Full Middleware Pipeline
    // =========================================================================
    let tenant_resolver =
        HeaderTenantResolver::new().add_tenant(Tenant::new("acme-corp", "acme-corp"));

    let auth_scheme = BearerTokenScheme::new(|token: String| async move {
        match token.as_str() {
            "valid-admin-token" => Ok(Some(
                AuthUser::new("admin_user_42")
                    .with_role("operator")
                    .with_permission("orders:create"),
            )),
            "viewer-token" => Ok(Some(
                AuthUser::new("viewer_user_7")
                    .with_role("viewer")
                    .with_permission("orders:view"),
            )),
            _ => Ok(None),
        }
    });

    let app = Router::new()
        .route("/api/v1/orders", post(enterprise_create_order_endpoint))
        .layer(PolicyLayer::new(RequirePermission("orders:create")))
        .layer(AuthLayer::required().scheme(auth_scheme))
        .layer(TenantLayer::required(tenant_resolver))
        .layer(ObservabilityLayer::new())
        .with_state(UnifiedAppState {
            pool: Arc::clone(&pool),
        });

    // =========================================================================
    // 3. HTTP Request Validation Through Full Middleware Stack & Observability
    // =========================================================================
    // Case 3a: Missing tenant header -> 400 Bad Request
    {
        let req = Request::builder()
            .uri("/api/v1/orders")
            .method("POST")
            .header("content-type", "application/json")
            .header("authorization", "Bearer valid-admin-token")
            .body(Body::from(r#"{"order_id":"test_1","amount_cents":1000}"#))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        assert!(
            resp.headers().contains_key("x-request-id"),
            "x-request-id must be present on failure"
        );
        assert_eq!(
            resp.headers().get("content-type").unwrap(),
            "application/problem+json",
            "failure responses must adhere to RFC 9457 problem details"
        );
    }

    // Case 3b: Missing or invalid authentication token -> 401 Unauthorized
    {
        let req = Request::builder()
            .uri("/api/v1/orders")
            .method("POST")
            .header("content-type", "application/json")
            .header("x-tenant-id", "acme-corp")
            .header("authorization", "Bearer bogus-token")
            .body(Body::from(r#"{"order_id":"test_2","amount_cents":1000}"#))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        assert!(resp.headers().contains_key("x-request-id"));
        assert_eq!(
            resp.headers().get("content-type").unwrap(),
            "application/problem+json"
        );
    }

    // Case 3c: Authenticated user lacks required permission -> 403 Forbidden
    {
        let req = Request::builder()
            .uri("/api/v1/orders")
            .method("POST")
            .header("content-type", "application/json")
            .header("x-tenant-id", "acme-corp")
            .header("authorization", "Bearer viewer-token")
            .body(Body::from(r#"{"order_id":"test_3","amount_cents":1000}"#))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        assert!(resp.headers().contains_key("x-request-id"));
        assert_eq!(
            resp.headers().get("content-type").unwrap(),
            "application/problem+json"
        );
    }

    // Case 3d: Valid request passes all middleware layers -> 201 Created
    let order_id = "ord_pg_unified_001";
    {
        let payload = serde_json::json!({
            "order_id": order_id,
            "amount_cents": 25000
        });
        let req = Request::builder()
            .uri("/api/v1/orders")
            .method("POST")
            .header("content-type", "application/json")
            .header("x-tenant-id", "acme-corp")
            .header("authorization", "Bearer valid-admin-token")
            .body(Body::from(payload.to_string()))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        assert!(resp.headers().contains_key("x-request-id"));
    }

    // Verify transactional consistency: both business order and outbox message committed
    let order_status = pg_backend
        .query_scalar(&format!(
            "SELECT status FROM enterprise_orders WHERE id = '{order_id}';"
        ))
        .await
        .unwrap();
    assert_eq!(order_status.as_deref(), Some("created"));
    assert_eq!(pool.pending_count().await.unwrap(), 1);

    // =========================================================================
    // 4. Failure-Injected Relay Dispatch & Consumer-Side Idempotency
    // =========================================================================
    let queue_for_relay = Arc::clone(&queue);
    let simulated_drop_occurred = Arc::new(AtomicBool::new(false));
    let sim_drop_clone = Arc::clone(&simulated_drop_occurred);

    let relay = OutboxRelay::new(Arc::clone(&pg_backend), move |msg| {
        let q = Arc::clone(&queue_for_relay);
        let sim_drop = Arc::clone(&sim_drop_clone);
        let event: EnterpriseOrderCreatedEvent = msg.deserialize_payload().unwrap();
        async move {
            // Broker publish: Enqueue background fulfillment job
            let job = FulfillEnterpriseOrderJob {
                tenant_id: event.tenant_id,
                order_id: event.order_id,
            };
            q.enqueue(job)
                .await
                .map_err(|e| OutboxError::Repository(e.to_string()))?;

            // Simulate network disconnect AFTER successful broker enqueue on first attempt
            if !sim_drop.swap(true, Ordering::SeqCst) {
                return Err(OutboxError::Dispatch(
                    "simulated broker connection drop immediately post-enqueue".into(),
                ));
            }

            Ok(())
        }
    })
    .with_config(OutboxRelayConfig {
        batch_size: 10,
        lease_duration_secs: 2,
        poll_interval: Duration::from_millis(50),
        max_retries: 3,
    });

    // Pass 1: Fails after publish -> schedules retry with 1s backoff
    let poll1 = relay.poll_and_dispatch(5).await.unwrap();
    assert_eq!(poll1.published, 0);
    assert_eq!(poll1.retried, 1);
    assert!(simulated_drop_occurred.load(Ordering::SeqCst));

    // Wait for the 1-second backoff duration to elapse
    tokio::time::sleep(Duration::from_millis(1200)).await;

    // Pass 2: Retry succeeds -> enqueues second job (duplicate) and completes outbox
    let poll2 = relay.poll_and_dispatch(5).await.unwrap();
    assert_eq!(poll2.published, 1);
    assert_eq!(pool.pending_count().await.unwrap(), 0);

    // Verify background worker processes both duplicate jobs idempotently
    let fulfillment_executions = Arc::new(AtomicUsize::new(0));
    let notify_fulfillment = Arc::new(tokio::sync::Notify::new());

    let mut registry = JobRegistry::new();
    registry.register(IdempotentFulfillmentHandler {
        backend: Arc::clone(&pg_backend),
        executions: Arc::clone(&fulfillment_executions),
        notify: Arc::clone(&notify_fulfillment),
    });

    let worker = JobWorker::new(Arc::clone(&queue), Arc::new(registry));
    worker.start();

    // Await both duplicate job deliveries
    notify_fulfillment.notified().await;
    notify_fulfillment.notified().await;

    assert!(worker.drain(Duration::from_secs(3)).await);
    assert_eq!(fulfillment_executions.load(Ordering::SeqCst), 2);

    // Verify database state: Exactly ONE fulfillment record exists despite duplicate delivery!
    let fulfillment_count = pg_backend
        .query_scalar(&format!(
            "SELECT COUNT(*)::TEXT FROM enterprise_fulfillments WHERE order_id = '{order_id}';"
        ))
        .await
        .unwrap();
    assert_eq!(fulfillment_count.as_deref(), Some("1"));

    let dispatch_count = pg_backend
        .query_scalar(&format!(
            "SELECT dispatch_count::TEXT FROM enterprise_fulfillments WHERE order_id = '{order_id}';"
        ))
        .await
        .unwrap();
    assert_eq!(dispatch_count.as_deref(), Some("2"));

    // =========================================================================
    // 5. Worker Crash Simulation & Peer Lease Reclaim
    // =========================================================================
    let crash_test_job = FulfillEnterpriseOrderJob {
        tenant_id: "acme-corp".into(),
        order_id: "ord_crash_simulation_99".into(),
    };
    queue.enqueue(crash_test_job).await.unwrap();

    // Worker 1 claims the job with a 1500ms lease
    let job1 = queue
        .dequeue_for("worker_node_crash_1")
        .await
        .unwrap()
        .expect("job should be dequeued by worker 1");
    assert_eq!(job1.name, FulfillEnterpriseOrderJob::NAME);
    let token1 = job1.lease_token.clone().expect("lease token present");

    // Peer Worker 2 attempts to claim immediately -> must NOT steal active lease
    let premature_claim = queue.dequeue_for("worker_node_peer_2").await.unwrap();
    assert!(
        premature_claim.is_none(),
        "peer worker cannot claim a job under an active unexpired lease"
    );

    // Simulate Worker 1 crashing: sleep past the 1500ms lease duration
    tokio::time::sleep(Duration::from_millis(1700)).await;

    // Worker 2 reclaims the expired job
    let job2 = queue
        .dequeue_for("worker_node_peer_2")
        .await
        .unwrap()
        .expect("peer worker must reclaim the expired job");
    assert_eq!(job2.id, job1.id);
    let token2 = job2.lease_token.clone().expect("new lease token present");
    assert_ne!(
        token1, token2,
        "reclaimed job must be assigned a new lease token"
    );

    // Crashed Worker 1 attempts to ack with its stale lease -> must be rejected
    let stale_ack_res = queue.ack(&job1.id, &token1).await;
    assert!(
        stale_ack_res.is_err(),
        "crashed worker must not be permitted to ack after lease reclaim"
    );

    // Worker 2 acks successfully
    queue
        .ack(&job2.id, &token2)
        .await
        .expect("worker 2 ack must succeed");
    assert_eq!(queue.pending_count().await.unwrap(), 0);

    // =========================================================================
    // 6. Graceful Drain & Shutdown Under Active In-Flight Requests
    // =========================================================================
    let slow_job = SlowDrainJob {
        job_id: "slow_drain_001".into(),
    };
    queue.enqueue(slow_job).await.unwrap();

    let drain_completed = Arc::new(AtomicBool::new(false));
    let drain_started = Arc::new(tokio::sync::Notify::new());

    let mut drain_registry = JobRegistry::new();
    drain_registry.register(SlowDrainHandler {
        completed: Arc::clone(&drain_completed),
        started: Arc::clone(&drain_started),
    });

    let drain_worker = JobWorker::new(Arc::clone(&queue), Arc::new(drain_registry));
    drain_worker.start();

    // Wait until handler has begun executing in-flight
    drain_started.notified().await;

    // Initiate graceful worker drain while job is in-flight
    let worker_drained = drain_worker.drain(Duration::from_secs(3)).await;
    assert!(
        worker_drained,
        "worker must gracefully drain active in-flight jobs"
    );
    assert!(
        drain_completed.load(Ordering::SeqCst),
        "in-flight slow job must complete before drain finishes"
    );
    assert_eq!(queue.pending_count().await.unwrap(), 0);

    // Cleanly close and drain the database pool
    let pool_drained = pool.close_and_drain(Duration::from_secs(3)).await;
    assert!(
        pool_drained,
        "database pool must cleanly drain all active connections"
    );
}

#[tokio::test]
async fn test_postgres_phase5_resilience_matrix_disconnection_leader_crash_drain() {
    // Isolated PostgreSQL test container
    let (_db_name, pg_config) = common::containers::create_isolated_postgres_db().await;
    let pg_backend = Arc::new(
        PostgresBackend::connect(&pg_config)
            .await
            .expect("connect to isolated postgres"),
    );

    // Initialize required schemas: outbox, jobs, leader election
    pg_backend
        .init_outbox_table()
        .await
        .expect("init outbox table");
    let queue = Arc::new(
        PostgresJobQueue::new(pg_backend.pool().clone())
            .with_lease_duration(Duration::from_millis(1500)),
    );
    queue.init_schema().await.expect("init jobs schema");

    let leader_elector = PostgresLeaderElector::from_backend(&pg_backend);
    leader_elector
        .init_schema()
        .await
        .expect("init leader election schema");

    let pool = Arc::new(DatabasePool::new(
        (*pg_backend).clone(),
        DatabaseConfig::default(),
    ));

    // -------------------------------------------------------------------------
    // 1. Database Disconnection & Health Recovery Story
    // -------------------------------------------------------------------------
    let db_health = pool.health_check("postgres_db");
    let initial_health = db_health.run().await;
    assert_eq!(initial_health.status, rume::routing::HealthStatus::Ok);

    // Simulate database pool connection severance / shutdown
    pool.close_async().await;
    assert!(pool.status().is_closed);

    // Health check promptly reports degraded/failed state with diagnostics
    let degraded_health = db_health.run().await;
    assert_eq!(degraded_health.status, rume::routing::HealthStatus::Failed);
    assert!(degraded_health.message.contains("closed"));

    // Operations fail-fast with PersistenceError::Closed
    let lease_err = pool.acquire_lease().unwrap_err();
    assert!(matches!(lease_err, PersistenceError::Closed));

    // Recovery: Application establishes healthy reconnection pool
    let recovered_backend = Arc::new(
        PostgresBackend::connect(&pg_config)
            .await
            .expect("reconnect to postgres"),
    );
    let recovered_pool = Arc::new(DatabasePool::new(
        (*recovered_backend).clone(),
        DatabaseConfig::default(),
    ));
    let recovered_health = recovered_pool.health_check("postgres_db").run().await;
    assert_eq!(recovered_health.status, rume::routing::HealthStatus::Ok);

    // -------------------------------------------------------------------------
    // 2. Leader Lease Expiry and Takeover in Integrated Flow
    // -------------------------------------------------------------------------
    let recovered_leader_elector = PostgresLeaderElector::from_backend(&recovered_backend);
    let role = "order-fulfillment-coordinator";
    // Node A acquires leadership with a 1500ms lease
    let lease_a = recovered_leader_elector
        .try_acquire(role, "node-alpha", Duration::from_millis(1500))
        .await
        .expect("acquire leader")
        .expect("node-alpha should become leader");
    assert_eq!(lease_a.candidate_id, "node-alpha");
    assert_eq!(lease_a.term, 1);

    // Standby Node B attempts to claim leadership concurrently -> must be rejected
    let lease_b_premature = recovered_leader_elector
        .try_acquire(role, "node-beta", Duration::from_millis(1500))
        .await
        .expect("try acquire");
    assert!(
        lease_b_premature.is_none(),
        "standby node cannot steal active unexpired lease"
    );

    // Node A crashes / hangs (stops renewing) -> sleep past lease expiration
    tokio::time::sleep(Duration::from_millis(1700)).await;

    // Node B detects lease expiration and takes over leadership cleanly
    let lease_b = recovered_leader_elector
        .try_acquire(role, "node-beta", Duration::from_millis(1500))
        .await
        .expect("acquire expired")
        .expect("standby node must acquire expired leader lease");
    assert_eq!(lease_b.candidate_id, "node-beta");
    assert!(
        lease_b.term > lease_a.term,
        "term sequence must advance on failover"
    );

    // Crashed Node A wakes up and attempts renewal -> must be rejected
    let stale_renew = recovered_leader_elector
        .renew(role, "node-alpha", Duration::from_millis(1500))
        .await
        .expect("renew attempt");
    assert!(
        stale_renew.is_none(),
        "crashed leader must not be permitted to renew after peer takeover"
    );

    // -------------------------------------------------------------------------
    // 3. Relay Restart / Crash Recovery at Publication-to-Ack Boundary
    // -------------------------------------------------------------------------
    // Insert pending outbox message
    let test_order_event = EnterpriseOrderCreatedEvent {
        tenant_id: "acme-corp".into(),
        order_id: "ord_pub_ack_crash_99".into(),
        amount_cents: 9900,
    };
    let outbox_msg = OutboxMessage::from_event(&test_order_event).expect("outbox msg");
    recovered_pool
        .save(outbox_msg)
        .await
        .expect("save outbox message");
    assert_eq!(recovered_pool.pending_count().await.unwrap(), 1);

    let side_effects_count = Arc::new(AtomicUsize::new(0));
    let side_effects_clone = Arc::clone(&side_effects_count);

    // Relay 1 claims the message with a 1-second lease, runs dispatch, but crashes before mark_published
    let claimed_by_relay_1 = recovered_pool.claim_pending_with_lease(1, 1).await.unwrap();
    assert_eq!(claimed_by_relay_1.len(), 1);
    let msg_to_dispatch = &claimed_by_relay_1[0];
    assert_eq!(msg_to_dispatch.status, OutboxStatus::InFlight);

    // Simulate dispatch effect executing
    side_effects_clone.fetch_add(1, Ordering::SeqCst);
    // Crash simulation: Relay 1 terminates immediately WITHOUT calling mark_published!

    // Verify message remains inflight in database
    assert_eq!(recovered_pool.pending_count().await.unwrap(), 1);

    // Wait for the 1-second lease to expire
    tokio::time::sleep(Duration::from_millis(1200)).await;

    // Relay 2 starts up, recovers expired claims, and re-dispatches
    let recovered_claims = recovered_pool
        .recover_expired_claims(1)
        .await
        .expect("recover claims");
    assert_eq!(
        recovered_claims, 1,
        "relay 2 must recover abandoned in-flight claim"
    );

    // Relay 2 claims the recovered message
    let side_effects_for_relay_2 = Arc::clone(&side_effects_count);
    let relay_2 = OutboxRelay::new(Arc::clone(&recovered_backend), move |_msg| {
        let effects = Arc::clone(&side_effects_for_relay_2);
        async move {
            // Idempotent consumer: records duplicate delivery but does not re-apply side effect
            effects.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    })
    .with_config(OutboxRelayConfig {
        batch_size: 10,
        lease_duration_secs: 2,
        poll_interval: Duration::from_millis(50),
        max_retries: 3,
    });

    let poll_result = relay_2.poll_and_dispatch(5).await.expect("relay 2 poll");
    assert_eq!(poll_result.published, 1);
    assert_eq!(recovered_pool.pending_count().await.unwrap(), 0);
    assert_eq!(
        side_effects_count.load(Ordering::SeqCst),
        2,
        "consumer saw 2 dispatches across crash recovery"
    );

    // -------------------------------------------------------------------------
    // 4. Coordinated Application Shutdown Under Active Workers and Relays
    // -------------------------------------------------------------------------
    let recovered_queue = Arc::new(
        PostgresJobQueue::new(recovered_backend.pool().clone())
            .with_lease_duration(Duration::from_millis(1500)),
    );

    // Enqueue 2 slow background jobs
    recovered_queue
        .enqueue(SlowDrainJob {
            job_id: "coord_shutdown_job_1".into(),
        })
        .await
        .unwrap();
    recovered_queue
        .enqueue(SlowDrainJob {
            job_id: "coord_shutdown_job_2".into(),
        })
        .await
        .unwrap();

    let mut shutdown_registry = JobRegistry::new();
    let handler_completed = Arc::new(AtomicBool::new(false));
    let handler_started = Arc::new(tokio::sync::Notify::new());
    shutdown_registry.register(SlowDrainHandler {
        completed: Arc::clone(&handler_completed),
        started: Arc::clone(&handler_started),
    });

    let shutdown_worker = JobWorker::new(Arc::clone(&recovered_queue), Arc::new(shutdown_registry));
    shutdown_worker.start();

    // Start background relay with observable join handle
    let relay_handle = relay_2.start_with_config();

    // Initiate concurrent graceful drain of worker, relay, and database pool
    let (worker_ok, relay_ok, pool_ok) = tokio::join!(
        shutdown_worker.drain(Duration::from_secs(4)),
        relay_2.drain(Duration::from_secs(4)),
        recovered_pool.close_and_drain(Duration::from_secs(4)),
    );

    assert!(
        worker_ok,
        "worker must drain cleanly under active in-flight work"
    );
    assert!(relay_ok, "relay must drain cleanly during shutdown");
    assert!(
        pool_ok,
        "pool must cleanly drain connections during shutdown"
    );

    // Background relay task terminated cleanly
    relay_handle.abort();
}
