#![cfg(feature = "mysql")]

use std::sync::Arc;

use testcontainers_modules::mysql::Mysql;

use rume::outbox::{OutboxMessage, OutboxRepository, OutboxStatus, TransactionalOutbox};
use rume::persistence::{
    DatabaseBackend, DatabaseConfig, DatabasePool, DatabaseTransaction, MysqlBackend,
};

use crate::common;

async fn start_mysql_node() -> (
    &'static testcontainers_modules::testcontainers::ContainerAsync<Mysql>,
    MysqlBackend,
    DatabaseConfig,
) {
    let node = common::containers::shared_mysql().await;
    let (_db_name, mut config) = common::containers::create_isolated_mysql_db().await;
    config.max_connections = 5;
    let backend = MysqlBackend::new(&config).expect("create mysql backend");

    // Initialize outbox schema
    backend
        .execute(MysqlBackend::OUTBOX_TABLE_SCHEMA)
        .await
        .expect("init outbox schema");

    (node, backend, config)
}

#[tokio::test]
async fn test_mysql_pool_lifecycle_and_ping() {
    let (_node, backend, config) = start_mysql_node().await;

    backend.ping().await.expect("ping successful");

    let pool = DatabasePool::new(backend.clone(), config);
    let _lease = pool.acquire_lease().expect("acquire lease");
    assert_eq!(pool.status().active_connections, 1);
}

#[tokio::test]
async fn test_mysql_execute_and_query_scalar() {
    let (_node, backend, _config) = start_mysql_node().await;

    backend
        .execute("CREATE TABLE users (id INT PRIMARY KEY AUTO_INCREMENT, name VARCHAR(255) NOT NULL)")
        .await
        .expect("create table");

    let affected = backend
        .execute("INSERT INTO users (name) VALUES ('Alice'), ('Bob')")
        .await
        .expect("insert rows");
    assert_eq!(affected, 2);

    let count = backend
        .query_scalar("SELECT count(*) FROM users")
        .await
        .expect("query scalar")
        .expect("some count");
    assert_eq!(count, "2");

    let first_name = backend
        .query_scalar("SELECT name FROM users WHERE id = 1")
        .await
        .expect("query scalar")
        .expect("some name");
    assert_eq!(first_name, "Alice");
}

#[tokio::test]
async fn test_mysql_transaction_commit_and_rollback() {
    let (_node, backend, _config) = start_mysql_node().await;

    backend
        .execute("CREATE TABLE accounts (id VARCHAR(32) PRIMARY KEY, balance INT NOT NULL)")
        .await
        .expect("create accounts");

    // 1. Commit scenario
    let tx1 = backend.begin_transaction().await.expect("begin tx1");
    tx1.execute("INSERT INTO accounts (id, balance) VALUES ('acc_1', 100)")
        .await
        .expect("insert in tx1");
    tx1.commit().await.expect("commit tx1");

    let balance = backend
        .query_scalar("SELECT balance FROM accounts WHERE id = 'acc_1'")
        .await
        .expect("query balance")
        .expect("some balance");
    assert_eq!(balance, "100");

    // 2. Rollback scenario
    let tx2 = backend.begin_transaction().await.expect("begin tx2");
    tx2.execute("INSERT INTO accounts (id, balance) VALUES ('acc_2', 200)")
        .await
        .expect("insert in tx2");
    tx2.rollback().await.expect("rollback tx2");

    let acc2 = backend
        .query_scalar("SELECT count(*) FROM accounts WHERE id = 'acc_2'")
        .await
        .expect("query acc2")
        .expect("count");
    assert_eq!(acc2, "0");
}

#[tokio::test]
async fn test_mysql_transaction_drop_auto_rollback() {
    let (_node, backend, _config) = start_mysql_node().await;

    backend
        .execute("CREATE TABLE audit_log (msg VARCHAR(255) NOT NULL)")
        .await
        .expect("create audit_log");

    {
        let tx = backend.begin_transaction().await.expect("begin tx");
        tx.execute("INSERT INTO audit_log (msg) VALUES ('should be dropped')")
            .await
            .expect("insert in tx");
        // tx dropped here without commit
    }

    // Allow background async rollback to process
    tokio::task::yield_now().await;

    let count = backend
        .query_scalar("SELECT count(*) FROM audit_log")
        .await
        .expect("query count")
        .expect("count");
    assert_eq!(count, "0");
}

#[tokio::test]
async fn test_mysql_transactional_outbox_atomicity() {
    let (_node, backend, _config) = start_mysql_node().await;

    backend
        .execute("CREATE TABLE orders (id VARCHAR(64) PRIMARY KEY, status VARCHAR(32) NOT NULL)")
        .await
        .expect("create orders");

    // 1. Successful business mutation + outbox save in same transaction
    let tx = backend.begin_transaction().await.expect("begin tx");
    tx.execute("INSERT INTO orders (id, status) VALUES ('order_1', 'placed')")
        .await
        .expect("insert order");

    let msg1 = OutboxMessage::new(
        "msg_1".to_string(),
        "OrderPlaced".to_string(),
        b"{\"order_id\":\"order_1\"}".to_vec(),
    );
    backend.save_tx(&tx, msg1).await.expect("save outbox in tx");
    tx.commit().await.expect("commit tx");

    assert_eq!(backend.pending_count().await.expect("pending count"), 1);

    // 2. Failed business transaction rolls back outbox message as well
    let tx_fail = backend.begin_transaction().await.expect("begin tx fail");
    tx_fail
        .execute("INSERT INTO orders (id, status) VALUES ('order_2', 'placed')")
        .await
        .expect("insert order 2");

    let msg2 = OutboxMessage::new(
        "msg_2".to_string(),
        "OrderPlaced".to_string(),
        b"{\"order_id\":\"order_2\"}".to_vec(),
    );
    backend
        .save_tx(&tx_fail, msg2)
        .await
        .expect("save outbox 2 in tx");
    tx_fail.rollback().await.expect("rollback tx fail");

    assert_eq!(backend.pending_count().await.expect("pending count"), 1);
}

#[tokio::test]
async fn test_mysql_outbox_claims_skip_locked_and_lease() {
    let (_node, backend, _config) = start_mysql_node().await;

    let backend = Arc::new(backend);

    let base_time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    for i in 1..=10 {
        let mut msg = OutboxMessage::new(
            format!("msg_{i}"),
            "InventoryReserved".to_string(),
            format!("{{\"item\":{i}}}").into_bytes(),
        );
        msg.created_at = base_time + (i as u64 * 10);
        backend.save(msg).await.expect("save outbox message");
    }

    assert_eq!(backend.pending_count().await.expect("pending count"), 10);

    // Concurrent claims with SKIP LOCKED
    let b1 = Arc::clone(&backend);
    let b2 = Arc::clone(&backend);

    let handle1 = tokio::spawn(async move { b1.claim_pending_with_lease(5, 60).await.unwrap() });
    let handle2 = tokio::spawn(async move { b2.claim_pending_with_lease(5, 60).await.unwrap() });

    let (c1, c2) = tokio::join!(handle1, handle2);
    let c1 = c1.unwrap();
    let c2 = c2.unwrap();

    assert_eq!(c1.len(), 5);
    assert_eq!(c2.len(), 5);

    // Ensure completely disjoint claims (no duplicate processing)
    for m1 in &c1 {
        for m2 in &c2 {
            assert_ne!(m1.id, m2.id, "claims must be mutually exclusive");
        }
    }

    // Mark c1 published with lease tokens
    for m in &c1 {
        let token = m.lease_token.as_ref().unwrap();
        backend.mark_published(&m.id, token).await.expect("mark published");
    }

    assert_eq!(backend.published_count().await.expect("published count"), 5);
}

#[tokio::test]
async fn test_mysql_outbox_expired_claim_recovery() {
    let (_node, backend, _config) = start_mysql_node().await;

    let msg = OutboxMessage::new(
        "msg_timeout".to_string(),
        "PaymentProcessed".to_string(),
        b"{\"amount\":100}".to_vec(),
    );
    backend.save(msg).await.expect("save message");

    // Claim with 1 second lease
    let claimed = backend
        .claim_pending_with_lease(1, 1)
        .await
        .expect("claim message");
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].status, OutboxStatus::InFlight);

    // Expire lease deterministically in storage
    backend
        .execute("UPDATE rume_outbox SET lease_expires_at = 0 WHERE status = 'inflight'")
        .await
        .expect("expire lease");

    let recovered = backend
        .recover_expired_claims(1)
        .await
        .expect("recover claims");
    assert_eq!(recovered, 1);

    // Can be claimed again
    let reclaimed = backend
        .claim_pending_with_lease(1, 60)
        .await
        .expect("reclaim message");
    assert_eq!(reclaimed.len(), 1);
    assert_eq!(reclaimed[0].id, "msg_timeout");
}
