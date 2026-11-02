//! Comprehensive failure-oriented enterprise integration pipeline test suite.
//!
//! Verifies the end-to-end integration of:
//! Authentication -> Multi-Tenant Resolution -> Policy Authorization ->
//! Atomic Database Transaction + Outbox Enlistment -> Relay Event Dispatch ->
//! Background Worker Execution -> Tracing & Metrics -> Failure Recovery & Graceful Drain.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use rume::app::state::{Availability, AvailabilityState};
use rume::auth::identity::AuthUser;
use rume::auth::policy::{RequirePermission, authorize};
use rume::events::Event;
use rume::jobs::{Job, JobContext, JobError, JobQueue, JobRegistry, JobWorker, PostgresJobQueue};
use rume::metrics::MetricsRegistry;
use rume::outbox::{
    OutboxMessage, OutboxRelay, OutboxRepository, OutboxStatus, TransactionalOutbox,
};
use rume::persistence::{
    DatabaseBackend, DatabaseConfig, DatabasePool, DatabaseTransaction, PersistenceError,
    SqliteBackend,
};
use rume::tenant::{Tenant, TenantContext};
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::runners::AsyncRunner;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OrderBilledEvent {
    pub tenant_id: String,
    pub order_id: String,
    pub amount_cents: u64,
}

impl Event for OrderBilledEvent {
    const TYPE: &'static str = "billing.order_billed";
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FulfillOrderJob {
    pub tenant_id: String,
    pub order_id: String,
}

impl Job for FulfillOrderJob {
    const NAME: &'static str = "fulfillment.fulfill_order";
}

struct FulfillOrderHandler {
    jobs_processed: Arc<AtomicUsize>,
    metrics: Arc<MetricsRegistry>,
}

impl rume::jobs::JobHandler<FulfillOrderJob> for FulfillOrderHandler {
    fn handle<'a>(
        &'a self,
        _ctx: &'a JobContext,
        job: FulfillOrderJob,
    ) -> rume::jobs::BoxFuture<'a, Result<(), JobError>> {
        let jp = Arc::clone(&self.jobs_processed);
        let m = Arc::clone(&self.metrics);
        Box::pin(async move {
            assert_eq!(job.tenant_id, "acme-corp");
            assert_eq!(job.order_id, "ord_9901");
            jp.fetch_add(1, Ordering::SeqCst);
            m.record_http_request("POST", 201, Duration::from_millis(15));
            Ok(())
        })
    }
}

#[tokio::test]
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
    
    let pg_node = Postgres::default().start().await.expect("start postgres");
    let pg_port = pg_node.get_host_port_ipv4(5432).await.expect("port");
    let pg_url = format!("postgresql://postgres:postgres@127.0.0.1:{pg_port}/postgres");
    let pg_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(&pg_url)
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

    registry.register(FulfillOrderHandler {
        jobs_processed: Arc::clone(&jobs_processed),
        metrics: Arc::clone(&metrics),
    });

    let worker = JobWorker::new(Arc::clone(&queue), Arc::new(registry));
    worker.start();

    // Await background job execution
    for _ in 0..50 {
        if jobs_processed.load(Ordering::SeqCst) >= 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let drained = worker.drain(Duration::from_millis(200)).await;
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
