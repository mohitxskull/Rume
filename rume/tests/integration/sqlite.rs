#![cfg(feature = "sqlite")]

use std::sync::Arc;

use rume::outbox::{OutboxMessage, OutboxRelay, OutboxRepository, TransactionalOutbox};
use rume::persistence::{
    DatabaseBackend, DatabaseConfig, DatabasePool, DatabaseTransaction, Migration, Migrator,
    SqliteBackend,
};

#[tokio::test]
async fn sqlite_pool_lifecycle_and_ping() {
    let backend = SqliteBackend::in_memory().await.expect("in-memory sqlite should initialize");
    let pool = DatabasePool::new(
        backend,
        DatabaseConfig::new("sqlite::memory:")
            .with_min_connections(2)
            .with_max_connections(5),
    );

    pool.backend().ping().await.expect("ping should succeed");
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    let status = pool.status();
    assert_eq!(status.leased_connections, 0);
    assert_eq!(status.active_connections, 0);
    assert_eq!(status.max_connections, 5);

    let lease1 = pool.acquire_lease().expect("should acquire lease 1");
    let lease2 = pool.acquire_lease().expect("should acquire lease 2");
    assert_eq!(pool.status().leased_connections, 2);
    assert_eq!(pool.status().active_connections, 0);

    drop(lease1);
    drop(lease2);
    assert_eq!(pool.status().leased_connections, 0);
}

#[tokio::test]
async fn sqlite_migrations_and_rollback() {
    let backend = SqliteBackend::in_memory().await.expect("in-memory sqlite");

    let migrator = Migrator::new()
        .add_migration(
            Migration::from_sql(
                "001_create_accounts",
                "create accounts table",
                "CREATE TABLE accounts (id INTEGER PRIMARY KEY, balance INTEGER NOT NULL);",
            )
            .with_down("DROP TABLE accounts;"),
        )
        .add_migration(
            Migration::from_sql(
                "002_create_orders",
                "create orders table",
                "CREATE TABLE orders (id INTEGER PRIMARY KEY, account_id INTEGER NOT NULL REFERENCES accounts(id));",
            )
            .with_down("DROP TABLE orders;"),
        );

    // Apply migrations
    let summary = migrator
        .run_pending_backend(&backend)
        .await
        .expect("migrations should apply");
    assert_eq!(summary.applied.len(), 2);

    // Idempotent re-run
    let summary2 = migrator
        .run_pending_backend(&backend)
        .await
        .expect("re-run should succeed");
    assert_eq!(summary2.applied.len(), 0);

    // Rollback last migration (002_create_orders)
    let rolled_back = migrator
        .rollback_last_backend(&backend)
        .await
        .expect("rollback should succeed");
    assert_eq!(rolled_back.as_deref(), Some("002_create_orders"));

    // Verify 002 is now pending again
    let summary3 = migrator
        .run_pending_backend(&backend)
        .await
        .expect("re-apply should succeed");
    assert_eq!(summary3.applied.len(), 1);
    assert_eq!(summary3.applied[0], "002_create_orders");
}

#[tokio::test]
async fn sqlite_auto_migrate_applies_pending_migrations() {
    let backend = SqliteBackend::in_memory().await.expect("in-memory sqlite");
    let config = DatabaseConfig::default().with_auto_migrate(true);
    let pool = DatabasePool::new(backend, config);

    let migrator = Migrator::new().add_migration(Migration::from_sql(
        "001_create_items",
        "create items",
        "CREATE TABLE items (id INTEGER PRIMARY KEY);",
    ));

    // When auto_migrate is true:
    let res = pool
        .apply_auto_migrations(&migrator)
        .await
        .expect("apply auto migrations");
    assert!(res.is_some());
    let summary = res.unwrap();
    assert_eq!(summary.applied.len(), 1);

    // When auto_migrate is false:
    let backend2 = SqliteBackend::in_memory().await.expect("in-memory sqlite");
    let config2 = DatabaseConfig::default().with_auto_migrate(false);
    let pool2 = DatabasePool::new(backend2, config2);
    let res2 = pool2
        .apply_auto_migrations(&migrator)
        .await
        .expect("no-op");
    assert!(res2.is_none());
}

#[tokio::test]
async fn sqlite_explicit_transaction_commit_and_rollback() {
    let backend = SqliteBackend::in_memory().await.expect("in-memory sqlite");
    backend
        .execute("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL);")
        .await
        .expect("table should create");

    // 1. Transaction commits
    let tx1 = backend.begin_transaction().await.expect("begin tx");
    tx1.execute("INSERT INTO users (id, name) VALUES (1, 'Alice');")
        .await
        .expect("insert alice");
    tx1.execute("INSERT INTO users (id, name) VALUES (2, 'Bob');")
        .await
        .expect("insert bob");
    tx1.commit().await.expect("commit should succeed");

    let count = backend
        .query_scalar("SELECT count(*) FROM users;")
        .await
        .expect("query scalar")
        .expect("count present");
    assert_eq!(count, "2");

    // 2. Transaction rolls back
    let tx2 = backend.begin_transaction().await.expect("begin tx");
    tx2.execute("INSERT INTO users (id, name) VALUES (3, 'Charlie');")
        .await
        .expect("insert charlie");
    tx2.rollback().await.expect("rollback");

    let charlie_exists = backend
        .query_scalar("SELECT name FROM users WHERE id = 3;")
        .await
        .expect("query");
    assert_eq!(charlie_exists, None);

    // 3. Drop rolls back automatically
    {
        let tx3 = backend.begin_transaction().await.expect("begin tx");
        tx3.execute("INSERT INTO users (id, name) VALUES (4, 'Dave');")
            .await
            .expect("insert dave");
        // tx3 is dropped here without commit
    }

    let dave_exists = backend
        .query_scalar("SELECT name FROM users WHERE id = 4;")
        .await
        .expect("query");
    assert_eq!(dave_exists, None);
}

#[tokio::test]
async fn sqlite_transactional_outbox_atomicity_and_relay() {
    let backend = Arc::new(SqliteBackend::in_memory().await.expect("in-memory sqlite"));
    backend
        .init_outbox_table()
        .await
        .expect("outbox table should initialize");
    backend
        .execute("CREATE TABLE accounts (id INTEGER PRIMARY KEY, balance INTEGER NOT NULL);")
        .await
        .expect("table should create");

    // Scenario A: Rollback rolls back both business write and outbox message
    let tx_fail = backend.begin_transaction().await.expect("begin tx");
    tx_fail
        .execute("INSERT INTO accounts (id, balance) VALUES (10, 500);")
        .await
        .expect("account insert");

    let fail_msg = OutboxMessage::new(
        uuid::Uuid::new_v4().to_string(),
        "AccountCreated",
        b"account:10".to_vec(),
    );
    backend
        .save_tx(&tx_fail, fail_msg)
        .await
        .expect("save outbox in tx");

    tx_fail.rollback().await.expect("rollback tx");

    // Verify neither account nor outbox message was persisted
    let acc_count = backend
        .query_scalar("SELECT count(*) FROM accounts WHERE id = 10;")
        .await
        .expect("query")
        .unwrap();
    assert_eq!(acc_count, "0");

    let outbox_count = backend.pending_count().await.expect("pending count");
    assert_eq!(outbox_count, 0);

    // Scenario B: Commit commits both business write and outbox message
    let tx_ok = backend.begin_transaction().await.expect("begin tx");
    tx_ok
        .execute("INSERT INTO accounts (id, balance) VALUES (20, 1000);")
        .await
        .expect("account insert");

    let ok_msg = OutboxMessage::new(
        uuid::Uuid::new_v4().to_string(),
        "AccountCreated",
        b"account:20".to_vec(),
    );
    backend
        .save_tx(&tx_ok, ok_msg)
        .await
        .expect("save outbox in tx");

    tx_ok.commit().await.expect("commit tx");

    let acc_20 = backend
        .query_scalar("SELECT count(*) FROM accounts WHERE id = 20;")
        .await
        .expect("query")
        .unwrap();
    assert_eq!(acc_20, "1");

    let pending = backend.pending_count().await.expect("pending count");
    assert_eq!(pending, 1);

    // Scenario C: Relay dispatches message and marks published
    let dispatched = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let dispatched_clone = Arc::clone(&dispatched);

    let relay = OutboxRelay::new(Arc::clone(&backend), move |msg| {
        let d = Arc::clone(&dispatched_clone);
        let id = msg.id.clone();
        async move {
            d.lock().await.push(id);
            Ok(())
        }
    });

    let count = relay.poll_and_dispatch(10).await.expect("dispatch");
    assert_eq!(count, 1);
    assert_eq!(dispatched.lock().await.len(), 1);

    let remaining = backend.pending_count().await.expect("pending count");
    assert_eq!(remaining, 0);

    let published = backend.published_count().await.expect("published count");
    assert_eq!(published, 1);
}

#[tokio::test]
async fn sqlite_outbox_lease_expiry_rejects_completion_before_another_claims() {
    let backend = SqliteBackend::in_memory()
        .await
        .expect("in-memory sqlite backend");
    backend
        .init_outbox_table()
        .await
        .expect("outbox table should initialize");

    let msg = OutboxMessage::new("msg-sqlite-exp", "event.user", b"payload".to_vec());
    backend.save(msg).await.expect("save outbox message");

    // Claim with lease_duration_secs = 0 -> immediately expired against database clock
    let claimed = backend
        .claim_pending_with_lease(1, 0)
        .await
        .expect("claim message");
    assert_eq!(claimed.len(), 1);
    let token = claimed[0].lease_token.as_ref().unwrap();

    // Worker attempts mark_published after lease expired -> must be rejected
    let res = backend.mark_published("msg-sqlite-exp", token).await;
    assert!(
        res.is_err(),
        "mark_published must fail when lease has expired"
    );

    // Worker attempts mark_failed after lease expired -> must be rejected
    let fail_res = backend
        .mark_failed_with_max_retries("msg-sqlite-exp", token, "transient error".into(), 5)
        .await;
    assert!(
        fail_res.is_err(),
        "mark_failed must fail when lease has expired"
    );

    assert_eq!(backend.published_count().await.unwrap(), 0);
}
