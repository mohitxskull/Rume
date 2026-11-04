//! Production PostgreSQL integration tests using Testcontainers.
//!
//! Validates:
//! - Real PostgreSQL connection pool lifecycle, semaphore limits, and timeout behavior
//! - Schema migrations (up and down) and preflight verification
//! - Transaction-enlisted outbox atomicity (commit vs rollback)
//! - Concurrent row claiming using PostgreSQL `FOR UPDATE SKIP LOCKED`
//! - Lease expiration and crashed worker recovery
//! - End-to-end `OutboxRelay` -> `EventBus` delivery on live PostgreSQL

#![cfg(feature = "postgres")]

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use rume::coordination::{
    CoordinationError, DistributedLock, LeaderElector, PostgresDistributedLock,
    PostgresLeaderElector,
};
use rume::events::{Event, EventBus};
use rume::jobs::{
    BoxFuture, Job, JobContext, JobError, JobHandler, JobQueue, JobRegistry, JobWorker,
    PostgresJobQueue,
};
use rume::outbox::{
    OutboxMessage, OutboxRelay, OutboxRelayConfig, OutboxRepository, TransactionalOutbox,
};
use rume::persistence::error::PersistenceError;
use rume::persistence::migration::{Migration, Migrator};
use rume::persistence::pool::DatabasePool;
use rume::persistence::postgres::PostgresBackend;
use rume::persistence::{DatabaseBackend, DatabaseTransaction};
use rume::routing::health::{HealthCheck, HealthStatus};

use testcontainers_modules::postgres::Postgres;

use crate::common;

/// Helper to spin up a PostgreSQL 16 container and return a configured connection pool.
async fn setup_postgres_pool(
    max_connections: u32,
) -> (
    &'static testcontainers_modules::testcontainers::ContainerAsync<Postgres>,
    DatabasePool<PostgresBackend>,
) {
    let node = common::containers::shared_postgres().await;
    let (_db_name, pool) =
        common::containers::create_isolated_postgres_pool(max_connections).await;
    (node, pool)
}

#[tokio::test]
async fn test_postgres_pool_lifecycle_and_health() {
    let (_container, pool) = setup_postgres_pool(4).await;

    // Verify database connectivity
    let health = pool.health_check("postgres").run().await;
    assert_eq!(health.status, HealthStatus::Ok, "pool should be healthy");

    // Acquire and release leases
    assert_eq!(pool.status().leased_connections, 0);
    {
        let lease1 = pool
            .acquire_lease_timeout(Duration::from_millis(500))
            .await
            .expect("should acquire lease 1");
        assert_eq!(pool.status().leased_connections, 1);

        let _lease2 = pool
            .acquire_lease_timeout(Duration::from_millis(500))
            .await
            .expect("should acquire lease 2");
        assert_eq!(pool.status().leased_connections, 2);

        drop(lease1);
        assert_eq!(pool.status().leased_connections, 1);
    }
    assert_eq!(pool.status().leased_connections, 0);

    // Verify graceful closure
    let closed = pool.close_and_drain(Duration::from_millis(500)).await;
    assert!(closed);
    assert!(pool.status().is_closed);
    assert!(pool.backend().is_closed());

    // Invariant: leases must be rejected after pool closure
    assert!(matches!(pool.acquire_lease(), Err(PersistenceError::Closed)));
    assert!(matches!(
        pool.acquire_lease_timeout(Duration::from_millis(50)).await,
        Err(PersistenceError::Closed)
    ));

    // Invariant: direct queries against the closed physical backend pool must fail
    let query_res = pool.backend().execute("SELECT 1").await;
    assert!(
        matches!(query_res, Err(PersistenceError::Closed)),
        "queries against closed pool must fail with PersistenceError::Closed"
    );
}

#[tokio::test]
async fn test_postgres_migrations_up_and_rollback() {
    let (_container, pool) = setup_postgres_pool(4).await;
    let backend = pool.backend();

    let migrator = Migrator::new()
        .add_migration(
            Migration::from_sql(
                "20261102_001_accounts",
                "create accounts table",
                "CREATE TABLE accounts (id TEXT PRIMARY KEY, balance INT NOT NULL)",
            )
            .with_down("DROP TABLE accounts"),
        )
        .add_migration(
            Migration::from_sql(
                "20261102_002_audit",
                "create audit table",
                "CREATE TABLE audit_logs (id TEXT PRIMARY KEY, action TEXT NOT NULL)",
            )
            .with_down("DROP TABLE audit_logs"),
        );

    // Preflight should detect pending migrations
    let preflight = migrator.verify_no_pending_backend(backend).await;
    assert!(preflight.is_err(), "should report pending migrations");
    if let Err(PersistenceError::PendingMigrations(pending)) = preflight {
        assert_eq!(pending.len(), 2);
    } else {
        panic!("unexpected error type: {preflight:?}");
    }

    // Execute migrations
    let summary = migrator
        .run_pending_backend(backend)
        .await
        .expect("migrations should succeed");
    assert_eq!(summary.applied.len(), 2);

    // Preflight should now pass
    migrator
        .verify_no_pending_backend(backend)
        .await
        .expect("preflight should pass after running migrations");

    // Tampered migration with altered SQL must be rejected by checksum verification
    let tampered_migrator = Migrator::new()
        .add_migration(
            Migration::from_sql(
                "20261102_001_accounts",
                "create accounts table with altered column",
                "CREATE TABLE accounts (id TEXT PRIMARY KEY, balance INT NOT NULL, extra INT)",
            )
            .with_down("DROP TABLE accounts"),
        )
        .add_migration(
            Migration::from_sql(
                "20261102_002_audit",
                "create audit table",
                "CREATE TABLE audit_logs (id TEXT PRIMARY KEY, action TEXT NOT NULL)",
            )
            .with_down("DROP TABLE audit_logs"),
        );
    let tampered_check = tampered_migrator.verify_no_pending_backend(backend).await;
    assert!(
        matches!(
            tampered_check,
            Err(PersistenceError::MigrationChecksumMismatch { ref migration, .. })
            if migration == "20261102_001_accounts"
        ),
        "preflight must fail on checksum mismatch: {tampered_check:?}"
    );

    // Write and verify business data
    backend
        .execute("INSERT INTO accounts (id, balance) VALUES ('acc-100', 4200)")
        .await
        .expect("insert into accounts should succeed");

    let balance = backend
        .query_scalar("SELECT balance::text FROM accounts WHERE id = 'acc-100'")
        .await
        .expect("query scalar should succeed");
    assert_eq!(balance.as_deref(), Some("4200"));

    // Rollback last migration (audit_logs)
    let rolled_back = migrator
        .rollback_last_backend(backend)
        .await
        .expect("rollback should succeed");
    assert_eq!(rolled_back.as_deref(), Some("20261102_002_audit"));

    // Accounts table must still be intact
    let balance_after = backend
        .query_scalar("SELECT balance::text FROM accounts WHERE id = 'acc-100'")
        .await
        .expect("accounts query should succeed after rolling back audit table");
    assert_eq!(balance_after.as_deref(), Some("4200"));
}

#[tokio::test]
async fn test_postgres_transactional_outbox_atomicity() {
    let (_container, pool) = setup_postgres_pool(4).await;
    let backend = pool.backend();

    // Initialize schema
    backend
        .init_outbox_table()
        .await
        .expect("init outbox table failed");
    backend
        .execute("CREATE TABLE orders (id TEXT PRIMARY KEY, amount INT NOT NULL)")
        .await
        .expect("create orders table failed");

    let backend_arc = Arc::new(backend.clone());

    // --- CASE A: Rollback discards both business write and outbox event ---
    {
        let b = Arc::clone(&backend_arc);
        let res = pool
            .transaction(move |tx| {
                let b = Arc::clone(&b);
                async move {
                    tx.execute("INSERT INTO orders (id, amount) VALUES ('ord-abort', 99)")
                        .await?;

                    let msg = OutboxMessage::new(
                        "outbox-abort",
                        "order.created",
                        br#"{"order_id":"ord-abort"}"#.to_vec(),
                    );
                    b.save_tx(&tx, msg)
                        .await
                        .map_err(|e| PersistenceError::QueryFailed(e.to_string()))?;

                    // Intentionally trigger rollback
                    Err::<(), PersistenceError>(PersistenceError::QueryFailed(
                        "force rollback".into(),
                    ))
                }
            })
            .await;
        assert!(res.is_err(), "transaction should return error on abort");

        // Verify neither exists
        let order = backend
            .query_scalar("SELECT id FROM orders WHERE id = 'ord-abort'")
            .await
            .expect("query failed");
        assert_eq!(order, None, "aborted order must not exist");

        let pending_count = backend.pending_count().await.expect("pending count failed");
        assert_eq!(pending_count, 0, "aborted outbox message must not exist");
    }

    // --- CASE B: Commit commits both business write and outbox event ---
    {
        let b = Arc::clone(&backend_arc);
        let res = pool
            .transaction(move |tx| {
                let b = Arc::clone(&b);
                async move {
                    tx.execute("INSERT INTO orders (id, amount) VALUES ('ord-commit', 150)")
                        .await?;

                    let msg = OutboxMessage::new(
                        "outbox-commit",
                        "order.created",
                        br#"{"order_id":"ord-commit"}"#.to_vec(),
                    );
                    b.save_tx(&tx, msg)
                        .await
                        .map_err(|e| PersistenceError::QueryFailed(e.to_string()))?;

                    Ok::<(), PersistenceError>(())
                }
            })
            .await;
        assert!(res.is_ok(), "transaction should succeed on commit");

        // Verify both exist
        let order = backend
            .query_scalar("SELECT id FROM orders WHERE id = 'ord-commit'")
            .await
            .expect("query failed");
        assert_eq!(order.as_deref(), Some("ord-commit"));

        let pending_count = backend.pending_count().await.expect("pending count failed");
        assert_eq!(pending_count, 1, "committed outbox message must exist");
    }
}

#[tokio::test]
async fn test_postgres_outbox_concurrency_skip_locked() {
    let (_container, pool) = setup_postgres_pool(8).await;
    let backend = pool.backend();
    backend
        .init_outbox_table()
        .await
        .expect("init outbox table failed");

    // Enqueue 10 pending outbox messages
    for i in 0..10 {
        let msg = OutboxMessage::new(
            format!("msg-{i:02}"),
            "event.batch",
            format!(r#"{{"idx":{i}}}"#).into_bytes(),
        );
        backend.save(msg).await.expect("save failed");
    }

    assert_eq!(backend.pending_count().await.unwrap(), 10);

    // Concurrently claim 5 messages each from two worker tasks
    let backend1 = backend.clone();
    let backend2 = backend.clone();

    let task1 = tokio::spawn(async move {
        backend1
            .claim_pending_with_lease(5, 30)
            .await
            .expect("worker 1 claim failed")
    });

    let task2 = tokio::spawn(async move {
        backend2
            .claim_pending_with_lease(5, 30)
            .await
            .expect("worker 2 claim failed")
    });

    let (batch1, batch2) = tokio::try_join!(task1, task2).expect("tasks failed");

    assert_eq!(batch1.len(), 5, "worker 1 should claim exactly 5 messages");
    assert_eq!(batch2.len(), 5, "worker 2 should claim exactly 5 messages");

    let ids1: HashSet<String> = batch1.iter().map(|m| m.id.clone()).collect();
    let ids2: HashSet<String> = batch2.iter().map(|m| m.id.clone()).collect();

    // Verify strict mutual exclusion: no message was claimed by both workers
    let intersection: HashSet<_> = ids1.intersection(&ids2).collect();
    assert!(
        intersection.is_empty(),
        "claimed batches must not overlap due to FOR UPDATE SKIP LOCKED, got: {intersection:?}"
    );

    // All 10 are now inflight; mark them published with their lease tokens
    for m in batch1.into_iter().chain(batch2.into_iter()) {
        let token = m.lease_token.as_ref().unwrap();
        backend
            .mark_published(&m.id, token)
            .await
            .expect("mark published failed");
    }

    assert_eq!(backend.pending_count().await.unwrap(), 0);
    assert_eq!(backend.published_count().await.unwrap(), 10);
}

#[tokio::test]
async fn test_postgres_outbox_expired_claim_recovery() {
    let (_container, pool) = setup_postgres_pool(4).await;
    let backend = pool.backend();
    backend
        .init_outbox_table()
        .await
        .expect("init outbox table failed");

    let msg = OutboxMessage::new("crashed-worker-msg", "job.event", b"payload data".to_vec());
    backend.save(msg).await.expect("save failed");

    // Claim with 0s lease duration -> immediately eligible for recovery
    let claimed = backend
        .claim_pending_with_lease(1, 0)
        .await
        .expect("claim failed");
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].id, "crashed-worker-msg");

    // At this moment, message is inflight (not pending)
    let pending_before = backend
        .fetch_pending(10)
        .await
        .expect("fetch pending failed");
    assert!(
        pending_before.is_empty(),
        "in-flight message should not be in pending list"
    );

    // Recover expired claims
    let recovered = backend
        .recover_expired_claims(0)
        .await
        .expect("recover expired failed");
    assert_eq!(recovered, 1, "exactly 1 expired claim should be recovered");

    // Re-claimed by a surviving worker
    let reclaimed = backend
        .claim_pending_with_lease(1, 60)
        .await
        .expect("reclaim failed");
    assert_eq!(reclaimed.len(), 1);
    assert_eq!(reclaimed[0].id, "crashed-worker-msg");

    let token = reclaimed[0].lease_token.as_ref().unwrap();
    backend
        .mark_published(&reclaimed[0].id, token)
        .await
        .expect("mark published failed");
    assert_eq!(backend.published_count().await.unwrap(), 1);
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
struct OrderPlacedEvent {
    order_id: String,
    amount: u32,
}

impl Event for OrderPlacedEvent {
    const TYPE: &'static str = "order.placed";
}

#[tokio::test]
async fn test_postgres_outbox_relay_pipeline_with_event_bus() {
    let (_container, pool) = setup_postgres_pool(4).await;
    let backend = pool.backend();
    backend
        .init_outbox_table()
        .await
        .expect("init outbox table failed");

    let event_bus = Arc::new(EventBus::new());
    let events_received = Arc::new(AtomicUsize::new(0));

    let er = Arc::clone(&events_received);
    event_bus.subscribe_fn(move |ev: &OrderPlacedEvent| {
        let count = Arc::clone(&er);
        let id = ev.order_id.clone();
        async move {
            assert!(id.starts_with("relay-ord-"));
            count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    });

    let eb_clone = Arc::clone(&event_bus);
    let relay = OutboxRelay::new(Arc::new(backend.clone()), move |msg| {
        let bus = Arc::clone(&eb_clone);
        let ev: OrderPlacedEvent = msg
            .deserialize_payload()
            .expect("deserialize payload failed");
        async move {
            bus.publish_sync(&ev).await.expect("publish event failed");
            Ok(())
        }
    });

    // Enqueue 3 messages
    for i in 1..=3 {
        let ev = OrderPlacedEvent {
            order_id: format!("relay-ord-{i}"),
            amount: i * 50,
        };
        let msg = OutboxMessage::from_event(&ev).expect("from_event failed");
        backend.save(msg).await.expect("save failed");
    }

    // Drain outbox relay
    let drained = relay.drain(Duration::from_secs(5)).await;
    assert!(drained, "relay should drain all messages");

    // Verify subscriber received all 3 events
    assert_eq!(events_received.load(Ordering::SeqCst), 3);

    // Verify outbox counts in live PostgreSQL
    assert_eq!(backend.published_count().await.unwrap(), 3);
    assert_eq!(backend.pending_count().await.unwrap(), 0);
}

#[tokio::test]
async fn test_postgres_pool_connection_limits_and_timeout() {
    let (_container, pool) = setup_postgres_pool(2).await;

    // Acquire all available connections (max = 2)
    let lease1 = pool
        .acquire_lease_timeout(Duration::from_millis(500))
        .await
        .expect("should acquire lease 1");
    let lease2 = pool
        .acquire_lease_timeout(Duration::from_millis(500))
        .await
        .expect("should acquire lease 2");

    // Attempting 3rd lease with timeout should fail with PersistenceError::Timeout
    let attempt = pool.acquire_lease_timeout(Duration::from_millis(100)).await;
    match attempt {
        Err(PersistenceError::Timeout) => {}
        other => panic!("expected PersistenceError::Timeout, got {other:?}"),
    }

    // Drop one lease, then acquisition succeeds
    drop(lease1);

    let lease3 = pool
        .acquire_lease_timeout(Duration::from_millis(500))
        .await
        .expect("should acquire lease 3 after dropping lease 1");

    drop(lease2);
    drop(lease3);
}

#[tokio::test]
async fn test_postgres_distributed_lock_mutual_exclusion_and_fencing() {
    let (_container, pool) = setup_postgres_pool(4).await;
    let lock_svc = PostgresDistributedLock::from_backend(pool.backend());
    lock_svc
        .init_schema()
        .await
        .expect("init lock schema failed");

    // Worker 1 acquires lock
    let guard1 = lock_svc
        .acquire("billing-reconciliation", Duration::from_secs(3))
        .await
        .expect("acquire call failed")
        .expect("worker 1 should acquire lock");

    assert_eq!(guard1.resource, "billing-reconciliation");
    assert!(
        guard1.fencing_token >= 1,
        "fencing token must be at least 1"
    );

    // Worker 2 attempts to acquire same resource while held -> must return None
    let attempt2 = lock_svc
        .acquire("billing-reconciliation", Duration::from_secs(3))
        .await
        .expect("acquire call failed");
    assert!(
        attempt2.is_none(),
        "worker 2 must not acquire active lock held by worker 1"
    );

    // Worker 1 renews lease
    lock_svc
        .renew(&guard1, Duration::from_secs(3))
        .await
        .expect("renewal should succeed");

    // Worker 1 releases lock
    let released = lock_svc
        .release(guard1.clone())
        .await
        .expect("release call failed");
    assert!(released, "lock release should report true");

    // Now Worker 2 acquires lock -> receives strictly higher monotonic fencing token
    let guard2 = lock_svc
        .acquire("billing-reconciliation", Duration::from_secs(3))
        .await
        .expect("acquire call failed")
        .expect("worker 2 should now acquire lock");

    assert!(
        guard2.fencing_token > guard1.fencing_token,
        "fencing token must strictly increase: {} > {}",
        guard2.fencing_token,
        guard1.fencing_token
    );

    lock_svc.release(guard2).await.expect("release failed");
}

#[tokio::test]
async fn test_postgres_distributed_lock_ttl_expiration_recovery() {
    let (_container, pool) = setup_postgres_pool(4).await;
    let lock_svc = PostgresDistributedLock::from_backend(pool.backend());
    lock_svc
        .init_schema()
        .await
        .expect("init lock schema failed");

    // Acquire with short 100ms TTL
    let guard1 = lock_svc
        .acquire("ephemeral-job", Duration::from_millis(100))
        .await
        .expect("acquire failed")
        .expect("guard1");

    // Immediately after, acquisition fails
    let immediate = lock_svc
        .acquire("ephemeral-job", Duration::from_millis(100))
        .await
        .expect("acquire failed");
    assert!(immediate.is_none(), "lock is currently held");

    // Expire lock TTL deterministically in storage
    pool.backend()
        .execute("UPDATE rume_locks SET expires_at = 0 WHERE resource = 'ephemeral-job'")
        .await
        .expect("expire lock");

    // A competing worker acquires expired lock
    let guard2 = lock_svc
        .acquire("ephemeral-job", Duration::from_secs(2))
        .await
        .expect("acquire failed")
        .expect("worker 2 should acquire expired lock");

    assert!(
        guard2.fencing_token > guard1.fencing_token,
        "fencing token must increase on expired lock steal"
    );

    // Original worker tries to renew expired guard -> fails with LockExpired
    let renew_attempt = lock_svc.renew(&guard1, Duration::from_secs(2)).await;
    match renew_attempt {
        Err(CoordinationError::LockExpired { resource }) => {
            assert_eq!(resource, "ephemeral-job");
        }
        other => panic!("expected LockExpired error, got: {other:?}"),
    }

    lock_svc.release(guard2).await.expect("release failed");
}

#[tokio::test]
async fn test_postgres_leader_election_failover_and_resignation() {
    let (_container, pool) = setup_postgres_pool(4).await;
    let elector = PostgresLeaderElector::from_backend(pool.backend());
    elector
        .init_schema()
        .await
        .expect("init leader schema failed");

    // Candidate A acquires leadership
    let lease_a = elector
        .try_acquire("cluster-coordinator", "node-alpha", Duration::from_secs(3))
        .await
        .expect("acquire failed")
        .expect("node-alpha should become leader");

    assert_eq!(lease_a.role, "cluster-coordinator");
    assert_eq!(lease_a.candidate_id, "node-alpha");
    assert!(lease_a.term >= 1);

    // Candidate B attempts to acquire -> returns None
    let lease_b_fail = elector
        .try_acquire("cluster-coordinator", "node-beta", Duration::from_secs(3))
        .await
        .expect("acquire failed");
    assert!(
        lease_b_fail.is_none(),
        "node-beta cannot acquire while alpha is leader"
    );

    // Query active leader
    let active = elector
        .get_leader("cluster-coordinator")
        .await
        .expect("get_leader failed")
        .expect("leader should exist");
    assert_eq!(active.candidate_id, "node-alpha");
    assert_eq!(active.term, lease_a.term);

    // Alpha renews leadership
    let renewed = elector
        .renew("cluster-coordinator", "node-alpha", Duration::from_secs(3))
        .await
        .expect("renew failed")
        .expect("renew should succeed");
    assert_eq!(renewed.term, lease_a.term);

    // Alpha voluntarily resigns
    let resigned = elector
        .resign("cluster-coordinator", "node-alpha")
        .await
        .expect("resign failed");
    assert!(resigned, "resignation should succeed");

    // Beta immediately acquires leadership with strictly higher term
    let lease_b = elector
        .try_acquire("cluster-coordinator", "node-beta", Duration::from_secs(3))
        .await
        .expect("acquire failed")
        .expect("node-beta should now become leader");

    assert_eq!(lease_b.candidate_id, "node-beta");
    assert!(
        lease_b.term > lease_a.term,
        "new term must be strictly greater: {} > {}",
        lease_b.term,
        lease_a.term
    );

    let active_after = elector
        .get_leader("cluster-coordinator")
        .await
        .expect("get_leader failed")
        .expect("leader should exist");
    assert_eq!(active_after.candidate_id, "node-beta");
    assert_eq!(active_after.term, lease_b.term);
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct SendInvoiceJob {
    invoice_id: String,
    amount: u32,
}

impl Job for SendInvoiceJob {
    const NAME: &'static str = "send_invoice";
}

#[tokio::test]
async fn test_postgres_job_queue_enqueue_dequeue_and_ack() {
    let (_container, pool) = setup_postgres_pool(4).await;
    let queue = PostgresJobQueue::from_backend(pool.backend());
    queue.init_schema().await.expect("init job schema failed");

    // Enqueue a job
    let job_id = queue
        .enqueue(SendInvoiceJob {
            invoice_id: "inv-101".into(),
            amount: 500,
        })
        .await
        .expect("enqueue failed");

    assert_eq!(queue.pending_count().await.unwrap(), 1);

    // Dequeue with specific worker ID
    let dequeued = queue
        .dequeue_for("worker-alpha")
        .await
        .expect("dequeue failed")
        .expect("job should be dequeued");

    assert_eq!(dequeued.id, job_id);
    assert_eq!(dequeued.name, "send_invoice");
    assert_eq!(dequeued.assigned_worker.as_deref(), Some("worker-alpha"));
    assert_eq!(dequeued.attempt, 1);

    // In-flight job is no longer pending
    assert_eq!(queue.pending_count().await.unwrap(), 0);

    // Ack marks job completed with valid lease token
    let token = dequeued.lease_token.as_deref().unwrap();
    queue.ack(&job_id, token).await.expect("ack failed");
    assert_eq!(queue.pending_count().await.unwrap(), 0);
}

#[tokio::test]
async fn test_postgres_job_queue_concurrency_skip_locked() {
    let (_container, pool) = setup_postgres_pool(8).await;
    let queue = PostgresJobQueue::from_backend(pool.backend());
    queue.init_schema().await.expect("init job schema failed");

    // Enqueue 10 jobs
    for i in 0..10 {
        queue
            .enqueue(SendInvoiceJob {
                invoice_id: format!("inv-{i}"),
                amount: i * 100,
            })
            .await
            .expect("enqueue failed");
    }

    assert_eq!(queue.pending_count().await.unwrap(), 10);

    let q1 = queue.clone();
    let q2 = queue.clone();

    // Two concurrent workers claiming 5 jobs each
    let worker1 = tokio::spawn(async move {
        let mut claimed = Vec::new();
        while claimed.len() < 5 {
            if let Some(job) = q1.dequeue_for("worker-1").await.unwrap() {
                claimed.push((job.id, job.lease_token.unwrap()));
            }
        }
        claimed
    });

    let worker2 = tokio::spawn(async move {
        let mut claimed = Vec::new();
        while claimed.len() < 5 {
            if let Some(job) = q2.dequeue_for("worker-2").await.unwrap() {
                claimed.push((job.id, job.lease_token.unwrap()));
            }
        }
        claimed
    });

    let (batch1, batch2) = tokio::try_join!(worker1, worker2).expect("workers failed");

    assert_eq!(batch1.len(), 5);
    assert_eq!(batch2.len(), 5);

    let ids1: HashSet<String> = batch1.iter().map(|(id, _)| id.clone()).collect();
    let ids2: HashSet<String> = batch2.iter().map(|(id, _)| id.clone()).collect();

    // Assert zero overlap due to FOR UPDATE SKIP LOCKED
    let overlap: HashSet<_> = ids1.intersection(&ids2).collect();
    assert!(
        overlap.is_empty(),
        "job claiming must not overlap, got: {overlap:?}"
    );

    // Ack all claimed jobs with their respective lease tokens
    for (id, token) in batch1.into_iter().chain(batch2.into_iter()) {
        queue.ack(&id, &token).await.expect("ack failed");
    }

    assert_eq!(queue.pending_count().await.unwrap(), 0);
}

#[tokio::test]
async fn test_postgres_job_queue_abandoned_worker_reclaim_and_dlq() {
    let (_container, pool) = setup_postgres_pool(4).await;
    let queue = PostgresJobQueue::from_backend(pool.backend());
    queue.init_schema().await.expect("init job schema failed");

    let job_id = queue
        .enqueue(SendInvoiceJob {
            invoice_id: "inv-crash".into(),
            amount: 999,
        })
        .await
        .expect("enqueue failed");

    // Dequeued by worker that crashes
    let claimed = queue
        .dequeue_for("crashed-worker-node")
        .await
        .expect("dequeue failed")
        .expect("job");
    assert_eq!(claimed.id, job_id);
    assert_eq!(queue.pending_count().await.unwrap(), 0);

    // Reclaim dead worker's jobs
    let dead_workers = vec!["crashed-worker-node".to_string()];
    let reclaimed = queue
        .reclaim_abandoned_jobs(&dead_workers)
        .await
        .expect("reclaim failed");
    assert_eq!(reclaimed, 1);

    // Job is now pending again
    assert_eq!(queue.pending_count().await.unwrap(), 1);

    // A healthy worker picks it up -> attempt count increments to 2
    let retried = queue
        .dequeue_for("healthy-worker-node")
        .await
        .expect("dequeue failed")
        .expect("job");
    assert_eq!(retried.id, job_id);
    assert_eq!(retried.attempt, 2);

    // Move to Dead-Letter Queue with lease token
    let retried_token = retried.lease_token.clone().unwrap();
    queue
        .dead_letter(retried, &retried_token, "database unrecoverable error".into())
        .await
        .expect("dead_letter failed");

    assert_eq!(queue.pending_count().await.unwrap(), 0);
    assert_eq!(queue.dead_letter_count().await.unwrap(), 1);
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct IdempotentChargeJob {
    charge_id: String,
}

impl Job for IdempotentChargeJob {
    const NAME: &'static str = "charge_card";

    fn idempotency_key(&self) -> Option<String> {
        Some(format!("idemp:{}", self.charge_id))
    }
}

#[tokio::test]
async fn test_postgres_job_queue_idempotency_deduplication() {
    let (_container, pool) = setup_postgres_pool(4).await;
    let queue = PostgresJobQueue::from_backend(pool.backend());
    queue.init_schema().await.expect("init job schema failed");

    let job = IdempotentChargeJob {
        charge_id: "charge-unique-42".into(),
    };

    // First enqueue succeeds
    let id1 = queue.enqueue(job.clone()).await.expect("enqueue 1 failed");

    // Second enqueue with identical idempotency key returns the existing job ID
    let id2 = queue.enqueue(job).await.expect("enqueue 2 failed");

    assert_eq!(id1, id2, "idempotent job must deduplicate to same ID");
    assert_eq!(
        queue.pending_count().await.unwrap(),
        1,
        "only 1 job should exist in queue"
    );
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
struct EnterpriseOrderCreatedEvent {
    order_id: String,
    tenant_id: String,
    amount: u32,
}

impl Event for EnterpriseOrderCreatedEvent {
    const TYPE: &'static str = "enterprise.order.created";
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct FulfillOrderJob {
    order_id: String,
    tenant_id: String,
}

impl Job for FulfillOrderJob {
    const NAME: &'static str = "fulfill_order";
}

#[tokio::test]
async fn test_postgres_realistic_enterprise_transaction_pipeline() {
    let (_container, pool) = setup_postgres_pool(6).await;
    let backend = pool.backend();

    // 1. Initialize schema
    backend
        .init_outbox_table()
        .await
        .expect("init outbox failed");

    let queue = Arc::new(PostgresJobQueue::from_backend(backend));
    queue.init_schema().await.expect("init job schema failed");

    backend
        .execute("CREATE TABLE IF NOT EXISTS enterprise_orders (id VARCHAR(255) PRIMARY KEY, tenant_id VARCHAR(255) NOT NULL, amount INT NOT NULL);")
        .await
        .expect("create orders table failed");

    // 2. Setup event bus and outbox relay
    let event_bus = Arc::new(EventBus::new());
    let queue_clone = Arc::clone(&queue);

    // Event subscriber enqueues background fulfillment job upon OrderCreated event
    event_bus.subscribe_fn(move |event: &EnterpriseOrderCreatedEvent| {
        let q = Arc::clone(&queue_clone);
        let order_id = event.order_id.clone();
        let tenant_id = event.tenant_id.clone();
        async move {
            q.enqueue(FulfillOrderJob {
                order_id,
                tenant_id,
            })
            .await
            .map_err(|e| rume::events::EventError::Bus(e.to_string()))?;
            Ok(())
        }
    });

    let event_bus_clone = Arc::clone(&event_bus);
    let relay = OutboxRelay::new(Arc::new(backend.clone()), move |msg| {
        let bus = Arc::clone(&event_bus_clone);
        let msg_clone = msg.clone();
        async move {
            let ev: EnterpriseOrderCreatedEvent = msg_clone
                .deserialize_payload()
                .map_err(|e| rume::outbox::OutboxError::Repository(e.to_string()))?;
            bus.publish_sync(&ev)
                .await
                .map_err(|e| rume::outbox::OutboxError::Repository(e.to_string()))?;
            Ok(())
        }
    });

    // 3. Execution: Successful enterprise transaction
    let tenant = "tenant-enterprise-alpha";
    let order_id = "order-tx-8888";

    let tx = backend.begin_transaction().await.expect("begin tx failed");
    tx.execute(&format!(
        "INSERT INTO enterprise_orders (id, tenant_id, amount) VALUES ('{order_id}', '{tenant}', 499);"
    ))
    .await
    .expect("insert order failed");

    let ev = EnterpriseOrderCreatedEvent {
        order_id: order_id.to_string(),
        tenant_id: tenant.to_string(),
        amount: 499,
    };
    let outbox_msg = OutboxMessage::from_event(&ev).expect("outbox message from event failed");
    backend
        .save_tx(&tx, outbox_msg)
        .await
        .expect("save outbox failed");

    tx.commit().await.expect("tx commit failed");

    // 4. Verify order is persisted in PostgreSQL
    let saved_order = backend
        .query_scalar(&format!(
            "SELECT id FROM enterprise_orders WHERE id = '{order_id}';"
        ))
        .await
        .expect("query scalar failed")
        .expect("order should exist");
    assert_eq!(saved_order, order_id);

    // 5. Drain outbox relay and dispatch event
    let dispatched_count = relay
        .poll_and_dispatch(10)
        .await
        .expect("relay dispatch failed");
    assert_eq!(dispatched_count, 1);
    assert_eq!(backend.pending_count().await.unwrap(), 0);
    assert_eq!(backend.published_count().await.unwrap(), 1);

    // 6. Verify job was enqueued in PostgreSQL job queue
    assert_eq!(queue.pending_count().await.unwrap(), 1);

    // 7. Worker claims job via SKIP LOCKED
    let queued_job = queue
        .dequeue_for("worker-node-1")
        .await
        .expect("dequeue failed")
        .expect("job should be dequeued");

    let job_payload: FulfillOrderJob = serde_json::from_slice(&queued_job.payload).unwrap();
    assert_eq!(job_payload.order_id, order_id);
    assert_eq!(job_payload.tenant_id, tenant);

    // Acknowledge job completion
    let job_token = queued_job.lease_token.as_deref().unwrap();
    queue.ack(&queued_job.id, job_token).await.expect("ack failed");
    assert_eq!(queue.pending_count().await.unwrap(), 0);

    // 8. Injected Failure Scenario: Rollback aborts both business write and outbox message
    let fail_tx = backend.begin_transaction().await.expect("begin fail tx");
    fail_tx
        .execute("INSERT INTO enterprise_orders (id, tenant_id, amount) VALUES ('order-fail-999', 'tenant-enterprise-alpha', 999);")
        .await
        .expect("insert fail order");

    let fail_ev = EnterpriseOrderCreatedEvent {
        order_id: "order-fail-999".to_string(),
        tenant_id: "tenant-enterprise-alpha".to_string(),
        amount: 999,
    };
    let fail_outbox = OutboxMessage::from_event(&fail_ev).expect("fail outbox");
    backend
        .save_tx(&fail_tx, fail_outbox)
        .await
        .expect("save fail outbox");

    fail_tx.rollback().await.expect("rollback failed");

    // Verify aborted order does NOT exist
    let aborted_order = backend
        .query_scalar("SELECT id FROM enterprise_orders WHERE id = 'order-fail-999';")
        .await
        .expect("query scalar");
    assert_eq!(aborted_order, None);

    // Outbox pending count remains 0
    assert_eq!(backend.pending_count().await.unwrap(), 0);
}

#[tokio::test]
async fn test_postgres_outbox_lease_expiry_rejects_completion_before_another_claims() {
    let (_container, pool) = setup_postgres_pool(4).await;
    let backend = pool.backend();
    backend
        .init_outbox_table()
        .await
        .expect("init outbox table failed");

    let msg = OutboxMessage::new("msg-pg-exp-1", "order.created", b"{}".to_vec());
    backend.save(msg).await.expect("save outbox message");

    // Claim with 0s lease duration -> immediately expired against EXTRACT(EPOCH FROM clock_timestamp())
    let claimed = backend
        .claim_pending_with_lease(1, 0)
        .await
        .expect("claim message");
    assert_eq!(claimed.len(), 1);
    let token = claimed[0].lease_token.as_ref().unwrap();

    // Worker attempts mark_published after lease expired -> must be rejected
    let res = backend.mark_published("msg-pg-exp-1", token).await;
    assert!(
        res.is_err(),
        "mark_published must fail when lease has expired in PostgreSQL"
    );

    // Worker attempts mark_failed after lease expired -> must be rejected
    let fail_res = backend
        .mark_failed_with_max_retries("msg-pg-exp-1", token, "transient error".into(), 5)
        .await;
    assert!(
        fail_res.is_err(),
        "mark_failed must fail when lease has expired in PostgreSQL"
    );

    assert_eq!(backend.published_count().await.unwrap(), 0);
}

#[tokio::test]
async fn test_postgres_outbox_superseded_worker_cannot_change_state_after_reclaim() {
    let (_container, pool) = setup_postgres_pool(4).await;
    let backend = pool.backend();
    backend
        .init_outbox_table()
        .await
        .expect("init outbox table failed");

    let msg = OutboxMessage::new("msg-pg-reclaim-1", "order.created", b"{}".to_vec());
    backend.save(msg).await.expect("save outbox message");

    // Worker A claims with 0s lease duration
    let claimed_a = backend
        .claim_pending_with_lease(1, 0)
        .await
        .expect("claim worker a");
    assert_eq!(claimed_a.len(), 1);
    let token_a = claimed_a[0].lease_token.as_ref().unwrap().clone();

    // Recovery runs and reclaims expired claim
    let recovered = backend.recover_expired_claims(0).await.expect("recover");
    assert_eq!(recovered, 1);

    // Worker B claims the message
    let claimed_b = backend
        .claim_pending_with_lease(1, 60)
        .await
        .expect("claim worker b");
    assert_eq!(claimed_b.len(), 1);
    let token_b = claimed_b[0].lease_token.as_ref().unwrap().clone();
    assert_ne!(token_a, token_b);

    // Worker A tries to mark published -> MUST be rejected (superseded!)
    let res_a = backend.mark_published("msg-pg-reclaim-1", &token_a).await;
    assert!(
        res_a.is_err(),
        "superseded worker A must not be allowed to mark published"
    );

    // Worker B marks published -> SUCCEEDS
    backend
        .mark_published("msg-pg-reclaim-1", &token_b)
        .await
        .expect("worker b mark published");
    assert_eq!(backend.published_count().await.unwrap(), 1);
}

#[tokio::test]
async fn test_postgres_job_queue_lease_expiry_rejects_ack_nack_dead_letter_before_reclaim() {
    let (_container, pool) = setup_postgres_pool(4).await;
    let queue = PostgresJobQueue::from_backend(pool.backend())
        .with_lease_duration(Duration::from_millis(100));
    queue.init_schema().await.expect("init job schema failed");

    // 1. Test ack rejection on expired lease
    let job1_id = queue
        .enqueue(SendInvoiceJob {
            invoice_id: "inv-exp-1".into(),
            amount: 100,
        })
        .await
        .expect("enqueue job 1");
    let job1 = queue.dequeue().await.unwrap().unwrap();
    let token1 = job1.lease_token.clone().unwrap();

    // Wait until lease expires (> 100ms)
    tokio::time::sleep(Duration::from_millis(250)).await;

    let ack_res = queue.ack(&job1_id, &token1).await;
    assert!(
        ack_res.is_err(),
        "ack must be rejected when lease has expired, even before peer worker claims"
    );

    // 2. Test nack rejection on expired lease
    let _job2_id = queue
        .enqueue(SendInvoiceJob {
            invoice_id: "inv-exp-2".into(),
            amount: 200,
        })
        .await
        .expect("enqueue job 2");
    let job2 = queue.dequeue().await.unwrap().unwrap();
    let token2 = job2.lease_token.clone().unwrap();

    tokio::time::sleep(Duration::from_millis(250)).await;

    let nack_res = queue.nack(job2, &token2, Duration::from_secs(1)).await;
    assert!(
        nack_res.is_err(),
        "nack must be rejected when lease has expired"
    );

    // 3. Test dead_letter rejection on expired lease
    let _job3_id = queue
        .enqueue(SendInvoiceJob {
            invoice_id: "inv-exp-3".into(),
            amount: 300,
        })
        .await
        .expect("enqueue job 3");
    let job3 = queue.dequeue().await.unwrap().unwrap();
    let token3 = job3.lease_token.clone().unwrap();

    tokio::time::sleep(Duration::from_millis(250)).await;

    let dl_res = queue.dead_letter(job3, &token3, "expired lease".into()).await;
    assert!(
        dl_res.is_err(),
        "dead_letter must be rejected when lease has expired"
    );
}

#[tokio::test]
async fn test_postgres_job_queue_superseded_worker_cannot_ack_after_peer_claims() {
    let (_container, pool) = setup_postgres_pool(4).await;
    let queue = PostgresJobQueue::from_backend(pool.backend())
        .with_lease_duration(Duration::from_millis(100));
    queue.init_schema().await.expect("init job schema failed");

    let job_id = queue
        .enqueue(SendInvoiceJob {
            invoice_id: "inv-peer-claim".into(),
            amount: 400,
        })
        .await
        .expect("enqueue job");

    // Worker A dequeues
    let job_a = queue.dequeue_for("worker-A").await.unwrap().unwrap();
    let token_a = job_a.lease_token.unwrap();

    // Sleep past lease duration
    tokio::time::sleep(Duration::from_millis(250)).await;

    // Worker B dequeues (claims after lease expired)
    let job_b = queue.dequeue_for("worker-B").await.unwrap().unwrap();
    let token_b = job_b.lease_token.unwrap();
    assert_eq!(job_b.id, job_id);
    assert_ne!(token_a, token_b);

    // Worker A attempts to ack -> rejected
    let ack_a = queue.ack(&job_id, &token_a).await;
    assert!(
        ack_a.is_err(),
        "superseded worker A must be rejected from acking"
    );

    // Worker B acks -> succeeds
    let ack_b = queue.ack(&job_id, &token_b).await;
    assert!(ack_b.is_ok(), "worker B ack must succeed");
}

#[tokio::test]
async fn test_postgres_job_queue_valid_lease_renewal_prevents_recovery() {
    let (_container, pool) = setup_postgres_pool(4).await;
    let queue = PostgresJobQueue::from_backend(pool.backend())
        .with_lease_duration(Duration::from_millis(400));
    queue.init_schema().await.expect("init job schema failed");

    let job_id = queue
        .enqueue(SendInvoiceJob {
            invoice_id: "inv-renew-prevents-recovery".into(),
            amount: 750,
        })
        .await
        .expect("enqueue job");

    let job = queue.dequeue_for("worker-renew").await.unwrap().unwrap();
    let token = job.lease_token.unwrap();

    // Before initial 400ms lease expires, renew it for 5 seconds
    tokio::time::sleep(Duration::from_millis(150)).await;
    queue
        .renew_lease(&job_id, &token, Duration::from_secs(5))
        .await
        .expect("renew lease should succeed");

    // Wait until initial 400ms lease would have expired (600ms total)
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Try to dequeue or reclaim abandoned jobs: another worker cannot claim because lease is still active
    let peer_dequeue = queue.dequeue_for("peer-worker").await.unwrap();
    assert!(
        peer_dequeue.is_none(),
        "renewed job lease must prevent other workers from reclaiming"
    );

    // Worker can still successfully ack with renewed lease
    let ack_res = queue.ack(&job_id, &token).await;
    assert!(ack_res.is_ok(), "ack with active renewed lease must succeed");
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct LongRunningWorkerJob {
    task_id: String,
}

impl Job for LongRunningWorkerJob {
    const NAME: &'static str = "long_running_worker_job";
}

struct LongRunningWorkerHandler {
    completed: Arc<AtomicUsize>,
}

impl JobHandler<LongRunningWorkerJob> for LongRunningWorkerHandler {
    fn handle<'a>(
        &'a self,
        _ctx: &'a JobContext,
        _job: LongRunningWorkerJob,
    ) -> BoxFuture<'a, Result<(), JobError>> {
        Box::pin(async move {
            tokio::time::sleep(Duration::from_millis(600)).await;
            self.completed.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }
}

#[tokio::test]
async fn test_postgres_job_worker_heartbeat_renewal_keeps_long_running_job_valid() {
    let (_container, pool) = setup_postgres_pool(4).await;
    // Configure a short lease of 300ms
    let queue = Arc::new(
        PostgresJobQueue::from_backend(pool.backend())
            .with_lease_duration(Duration::from_millis(300)),
    );
    queue.init_schema().await.expect("init job schema failed");

    let mut registry = JobRegistry::new();
    let job_completed = Arc::new(AtomicUsize::new(0));

    // Handler runs for 600ms (2x longer than the initial 300ms lease duration!)
    registry.register(LongRunningWorkerHandler {
        completed: Arc::clone(&job_completed),
    });

    let worker = JobWorker::new(Arc::clone(&queue), Arc::new(registry));
    worker.start();

    // Enqueue the long-running job
    queue
        .enqueue(LongRunningWorkerJob {
            task_id: "task-long-600".into(),
        })
        .await
        .expect("enqueue job");

    // Wait for the worker to finish the 600ms job + give a slight margin
    tokio::time::sleep(Duration::from_millis(900)).await;

    // Verify job handler completed
    assert_eq!(job_completed.load(Ordering::SeqCst), 1);

    // Verify worker status: no lost leases and no transition errors
    let status = worker.status();
    assert_eq!(
        status.transition_errors, 0,
        "worker must have 0 transition errors because heartbeat kept lease valid"
    );
    assert_eq!(
        status.lost_leases, 0,
        "worker must have 0 lost leases during heartbeat renewal"
    );

    // Verify job was acknowledged and is no longer pending
    assert_eq!(queue.pending_count().await.unwrap(), 0);
    assert!(worker.drain(Duration::from_secs(1)).await);
}

#[tokio::test]
async fn test_postgres_outbox_relay_heartbeat_renewal_keeps_long_running_dispatch_valid() {
    let (_container, pool) = setup_postgres_pool(4).await;
    let backend = Arc::new(pool.backend().clone());
    backend.init_outbox_table().await.expect("init outbox table failed");

    let event = OrderPlacedEvent {
        order_id: "order-relay-heartbeat-1".into(),
        amount: 9900,
    };
    let msg = OutboxMessage::from_event(&event).expect("outbox message");
    backend.save(msg).await.expect("save outbox message");

    // Configure relay with a 2-second lease
    let config = OutboxRelayConfig {
        batch_size: 10,
        lease_duration_secs: 2,
        poll_interval: Duration::from_millis(50),
        max_retries: 3,
    };

    let dispatched = Arc::new(AtomicUsize::new(0));
    let dispatched_clone = Arc::clone(&dispatched);

    // Dispatcher sleeps 2400ms (> initial 2s lease duration!)
    let relay = OutboxRelay::new(Arc::clone(&backend), move |_msg: &OutboxMessage| {
        let count = Arc::clone(&dispatched_clone);
        async move {
            tokio::time::sleep(Duration::from_millis(2400)).await;
            count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    })
    .with_config(config);

    let res = relay.poll_and_dispatch(10).await.expect("poll_and_dispatch failed");

    // Heartbeat kept lease renewed, so mark_published succeeded
    assert_eq!(res.published, 1);
    assert_eq!(res.failed_transitions, 0);
    assert_eq!(dispatched.load(Ordering::SeqCst), 1);
    assert_eq!(backend.pending_count().await.unwrap(), 0);
    assert_eq!(backend.published_count().await.unwrap(), 1);
}

#[tokio::test]
async fn test_postgres_outbox_batch_lease_expiry_and_pre_dispatch_renewal() {
    let (_container, pool) = setup_postgres_pool(4).await;
    let backend = Arc::new(pool.backend().clone());
    backend.init_outbox_table().await.expect("init outbox table failed");

    // Enqueue 2 messages
    let msg1 = OutboxMessage::new("msg-batch-1", "order.placed", b"{}".to_vec());
    let msg2 = OutboxMessage::new("msg-batch-2", "order.placed", b"{}".to_vec());
    backend.save(msg1).await.expect("save msg1");
    backend.save(msg2).await.expect("save msg2");

    // 2-second lease for claimed messages
    let config = OutboxRelayConfig {
        batch_size: 2,
        lease_duration_secs: 2,
        poll_interval: Duration::from_millis(50),
        max_retries: 3,
    };

    let processed = Arc::new(AtomicUsize::new(0));
    let processed_clone = Arc::clone(&processed);

    // Msg1 takes 600ms.
    // Msg2 sits in batch queue during msg1's dispatch, then takes 1600ms.
    // Total time elapsed since msg2 was claimed is 2200ms (> initial 2s lease).
    // Pre-dispatch renewal before msg2 starts dispatch refreshes msg2's lease!
    let relay = OutboxRelay::new(Arc::clone(&backend), move |msg: &OutboxMessage| {
        let count = Arc::clone(&processed_clone);
        let msg_id = msg.id.clone();
        async move {
            if msg_id == "msg-batch-1" {
                tokio::time::sleep(Duration::from_millis(600)).await;
            } else {
                tokio::time::sleep(Duration::from_millis(1600)).await;
            }
            count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    })
    .with_config(config);

    let res = relay.poll_and_dispatch(2).await.expect("poll_and_dispatch failed");

    assert_eq!(res.published, 2);
    assert_eq!(res.failed_transitions, 0);
    assert_eq!(processed.load(Ordering::SeqCst), 2);
    assert_eq!(backend.pending_count().await.unwrap(), 0);
    assert_eq!(backend.published_count().await.unwrap(), 2);
}

#[tokio::test]
async fn test_postgres_database_pool_outbox_relay_heartbeat_renewal_keeps_long_running_dispatch_valid() {
    let (_container, pool) = setup_postgres_pool(4).await;
    pool.backend().init_outbox_table().await.expect("init outbox table failed");
    let pool_arc = Arc::new(pool);

    let event = OrderPlacedEvent {
        order_id: "order-pool-relay-hb-1".into(),
        amount: 8800,
    };
    let msg = OutboxMessage::from_event(&event).expect("outbox message");
    pool_arc.save(msg).await.expect("save outbox message");

    let config = OutboxRelayConfig {
        batch_size: 10,
        lease_duration_secs: 2,
        poll_interval: Duration::from_millis(50),
        max_retries: 3,
    };

    let dispatched = Arc::new(AtomicUsize::new(0));
    let dispatched_clone = Arc::clone(&dispatched);

    // Dispatcher sleeps 2400ms (> initial 2s lease duration!)
    // Exercises lease renewal through DatabasePool<PostgresBackend> delegation
    let relay = OutboxRelay::new(Arc::clone(&pool_arc), move |_msg: &OutboxMessage| {
        let count = Arc::clone(&dispatched_clone);
        async move {
            tokio::time::sleep(Duration::from_millis(2400)).await;
            count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    })
    .with_config(config);

    let res = relay.poll_and_dispatch(10).await.expect("poll_and_dispatch failed");

    assert_eq!(res.published, 1);
    assert_eq!(res.failed_transitions, 0);
    assert_eq!(dispatched.load(Ordering::SeqCst), 1);
    assert_eq!(pool_arc.pending_count().await.unwrap(), 0);
    assert_eq!(pool_arc.published_count().await.unwrap(), 1);
}

#[tokio::test]
async fn test_postgres_parameterized_transaction_and_native_escape_hatch() {
    let (_container, pool) = setup_postgres_pool(4).await;
    let backend = pool.backend();

    backend
        .execute("CREATE TABLE test_params (id VARCHAR(64) PRIMARY KEY, title VARCHAR(255) NOT NULL, amount BIGINT, note TEXT);")
        .await
        .expect("create table");

    let tx = backend.begin_transaction().await.expect("begin tx");

    let injection_str = "'; DROP TABLE test_params; --";
    let unicode_note = "Safe 🚀 UTF-8: 'single quotes' and \"double quotes\"";

    let rows = tx
        .execute_params(
            "INSERT INTO test_params (id, title, amount, note) VALUES ($1, $2, $3, $4)",
            &rume::params![
                "row-1",
                injection_str,
                123456_i64,
                unicode_note,
            ],
        )
        .await
        .expect("execute_params should succeed");
    assert_eq!(rows, 1);

    // Insert with null amount
    let rows_null = tx
        .execute_params(
            "INSERT INTO test_params (id, title, amount, note) VALUES ($1, $2, $3, $4)",
            &rume::params![
                "row-2",
                "Second Row",
                None as Option<i64>,
                None as Option<String>,
            ],
        )
        .await
        .expect("insert null amount");
    assert_eq!(rows_null, 1);

    // Query scalar params inside transaction
    let retrieved = tx
        .query_scalar_params("SELECT title FROM test_params WHERE id = $1", &rume::params!["row-1"])
        .await
        .expect("query scalar")
        .expect("found title");
    assert_eq!(retrieved, injection_str);

    tx.commit().await.expect("commit tx");

    // Test raw_tx native escape hatch
    let tx_native = backend.begin_transaction().await.expect("begin native tx");
    let mut guard = tx_native.raw_tx().lock().await;
    let active_tx = guard.as_mut().expect("active tx present");
    sqlx::query("INSERT INTO test_params (id, title, amount, note) VALUES ($1, $2, $3, $4)")
        .bind("row-native")
        .bind("Native Driver Row")
        .bind(9999_i64)
        .bind("Direct SQLx Access")
        .execute(&mut **active_tx)
        .await
        .expect("native SQLx execute");
    drop(guard);
    tx_native.commit().await.expect("commit native tx");

    let count = backend
        .query_scalar("SELECT count(*) FROM test_params")
        .await
        .expect("count")
        .expect("count present");
    assert_eq!(count, "3");
}
