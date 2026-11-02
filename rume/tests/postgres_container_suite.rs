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

use rume::events::{Event, EventBus};
use rume::outbox::{OutboxMessage, OutboxRelay, OutboxRepository, TransactionalOutbox};
use rume::persistence::config::DatabaseConfig;
use rume::persistence::error::PersistenceError;
use rume::persistence::migration::{Migration, Migrator};
use rume::persistence::pool::DatabasePool;
use rume::persistence::postgres::PostgresBackend;
use rume::persistence::{DatabaseBackend, DatabaseTransaction};
use rume::routing::health::{HealthCheck, HealthStatus};

use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::ImageExt;
use testcontainers_modules::testcontainers::runners::AsyncRunner;

/// Helper to spin up a PostgreSQL 16 container and return a configured connection pool.
async fn setup_postgres_pool(
    max_connections: u32,
) -> (
    testcontainers_modules::testcontainers::ContainerAsync<Postgres>,
    DatabasePool<PostgresBackend>,
) {
    let container = Postgres::default()
        .with_tag("16-alpine")
        .start()
        .await
        .expect("Failed to start PostgreSQL container");

    let host = container
        .get_host()
        .await
        .expect("Failed to get container host");
    let port = container
        .get_host_port_ipv4(5432)
        .await
        .expect("Failed to get mapped port");

    let url = format!("postgres://postgres:postgres@{host}:{port}/postgres");
    let config = DatabaseConfig {
        url,
        max_connections,
        min_connections: 1,
        connect_timeout_secs: 5,
        idle_timeout_secs: Some(10),
        auto_migrate: false,
    };

    let backend = PostgresBackend::new(&config).expect("Failed to create PostgresBackend");
    let pool = DatabasePool::new(backend, config);

    (container, pool)
}

#[tokio::test]
async fn test_postgres_pool_lifecycle_and_health() {
    let (_container, pool) = setup_postgres_pool(4).await;

    // Verify database connectivity
    let health = pool.health_check("postgres").run().await;
    assert_eq!(health.status, HealthStatus::Ok, "pool should be healthy");

    // Acquire and release leases
    assert_eq!(pool.status().active_connections, 0);
    {
        let lease1 = pool
            .acquire_lease_timeout(Duration::from_millis(500))
            .await
            .expect("should acquire lease 1");
        assert_eq!(pool.status().active_connections, 1);

        let _lease2 = pool
            .acquire_lease_timeout(Duration::from_millis(500))
            .await
            .expect("should acquire lease 2");
        assert_eq!(pool.status().active_connections, 2);

        drop(lease1);
        assert_eq!(pool.status().active_connections, 1);
    }
    assert_eq!(pool.status().active_connections, 0);

    // Verify graceful closure
    let closed = pool.close_and_drain(Duration::from_millis(500)).await;
    assert!(closed);
    assert!(pool.status().is_closed);
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

    let ids1: HashSet<String> = batch1.into_iter().map(|m| m.id).collect();
    let ids2: HashSet<String> = batch2.into_iter().map(|m| m.id).collect();

    // Verify strict mutual exclusion: no message was claimed by both workers
    let intersection: HashSet<_> = ids1.intersection(&ids2).collect();
    assert!(
        intersection.is_empty(),
        "claimed batches must not overlap due to FOR UPDATE SKIP LOCKED, got: {intersection:?}"
    );

    // All 10 are now inflight; mark them published
    for id in ids1.union(&ids2) {
        backend
            .mark_published(id)
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

    backend
        .mark_published(&reclaimed[0].id)
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
