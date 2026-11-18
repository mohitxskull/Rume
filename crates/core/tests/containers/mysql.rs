#![cfg(feature = "mysql")]

use std::sync::Arc;

use testcontainers_modules::mysql::Mysql;

use rume::outbox::{OutboxMessage, OutboxRepository, OutboxStatus, TransactionalOutbox};
use rume::persistence::{
    DatabaseBackend, DatabaseConfig, DatabasePool, DatabaseTransaction, Migration, Migrator,
    MysqlBackend,
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
async fn test_mysql_unified_database_conformance_suite() {
    let (_node, backend, config) = start_mysql_node().await;
    let pool = DatabasePool::new(backend, config.with_max_connections(16));
    rume::testing::verify_database_conformance(&pool, "mysql")
        .await
        .expect("mysql database conformance suite must pass");
    pool.close_async().await;
}

#[tokio::test]
async fn test_mysql_pool_lifecycle_and_ping() {
    let (_node, backend, config) = start_mysql_node().await;

    backend.ping().await.expect("ping successful");

    let pool = DatabasePool::new(backend.clone(), config);
    let _lease = pool.acquire_lease().expect("acquire lease");
    assert_eq!(pool.status().leased_connections, 1);
}

#[tokio::test]
async fn test_mysql_execute_and_query_scalar() {
    let (_node, backend, _config) = start_mysql_node().await;

    backend
        .execute(
            "CREATE TABLE users (id INT PRIMARY KEY AUTO_INCREMENT, name VARCHAR(255) NOT NULL)",
        )
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
        backend
            .mark_published(&m.id, token)
            .await
            .expect("mark published");
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

#[tokio::test]
async fn test_mysql_outbox_lease_expiry_rejects_completion_before_another_claims() {
    let (_node, backend, _config) = start_mysql_node().await;
    backend
        .init_outbox_table()
        .await
        .expect("init outbox table failed");

    let msg = OutboxMessage::new("msg_mysql_exp", "order.created", b"{}".to_vec());
    backend.save(msg).await.expect("save outbox message");

    // Claim with lease_duration_secs = 0 -> immediately expired against UNIX_TIMESTAMP()
    let claimed = backend
        .claim_pending_with_lease(1, 0)
        .await
        .expect("claim message");
    assert_eq!(claimed.len(), 1);
    let token = claimed[0].lease_token.as_ref().unwrap();

    // Worker attempts mark_published after lease expired -> must be rejected
    let res = backend.mark_published("msg_mysql_exp", token).await;
    assert!(
        res.is_err(),
        "mark_published must fail when lease has expired in MySQL"
    );

    // Worker attempts mark_failed after lease expired -> must be rejected
    let fail_res = backend
        .mark_failed_with_max_retries("msg_mysql_exp", token, "transient db failure".into(), 5)
        .await;
    assert!(
        fail_res.is_err(),
        "mark_failed must fail when lease has expired in MySQL"
    );

    assert_eq!(backend.published_count().await.unwrap(), 0);
}

#[tokio::test]
async fn test_mysql_parameterized_transaction_and_native_escape_hatch() {
    let (_node, backend, _config) = start_mysql_node().await;

    backend
        .execute("CREATE TABLE test_mysql_params (id VARCHAR(64) PRIMARY KEY, title VARCHAR(255) NOT NULL, amount BIGINT, note TEXT);")
        .await
        .expect("create table");

    let tx = backend.begin_transaction().await.expect("begin tx");

    let injection_str = "'; DROP TABLE test_mysql_params; --";
    let unicode_note = "Safe 🚀 MySQL UTF-8: 'quotes' and \\backslashes\\";

    let rows = tx
        .execute_params(
            "INSERT INTO test_mysql_params (id, title, amount, note) VALUES (?, ?, ?, ?)",
            &rume::params!["row-1", injection_str, 54321_i64, unicode_note,],
        )
        .await
        .expect("execute_params should succeed");
    assert_eq!(rows, 1);

    // Insert with null amount and note
    let rows_null = tx
        .execute_params(
            "INSERT INTO test_mysql_params (id, title, amount, note) VALUES (?, ?, ?, ?)",
            &rume::params![
                "row-2",
                "Second MySQL Row",
                None as Option<i64>,
                None as Option<String>,
            ],
        )
        .await
        .expect("insert null values");
    assert_eq!(rows_null, 1);

    // Query scalar params inside transaction
    let retrieved = tx
        .query_scalar_params(
            "SELECT title FROM test_mysql_params WHERE id = ?",
            &rume::params!["row-1"],
        )
        .await
        .expect("query scalar")
        .expect("found title");
    assert_eq!(retrieved, injection_str);

    tx.commit().await.expect("commit tx");

    // Test raw_tx native escape hatch
    let tx_native = backend.begin_transaction().await.expect("begin native tx");
    let mut guard = tx_native.raw_tx().lock().await;
    let active_tx = guard.as_mut().expect("active tx present");
    sqlx::query("INSERT INTO test_mysql_params (id, title, amount, note) VALUES (?, ?, ?, ?)")
        .bind("row-native")
        .bind("Native Driver Row")
        .bind(8888_i64)
        .bind("Direct SQLx Access")
        .execute(&mut **active_tx)
        .await
        .expect("native SQLx execute");
    drop(guard);
    tx_native.commit().await.expect("commit native tx");

    let count = backend
        .query_scalar("SELECT count(*) FROM test_mysql_params")
        .await
        .expect("count")
        .expect("count present");
    assert_eq!(count, "3");
}

#[tokio::test]
async fn test_mysql_migration_lock_ordering_and_durability() {
    let (_node, backend, config) = start_mysql_node().await;

    let migrator = Migrator::new().add_migration(
        Migration::from_sql(
            "20261108_01_init",
            "create users table",
            "CREATE TABLE test_users (id INT PRIMARY KEY, name VARCHAR(50));",
        )
        .with_down("DROP TABLE test_users;"),
    );

    // 1. Run pending migrations: must acquire named lock, commit transaction, and release named lock
    let summary = migrator
        .run_pending_backend(&backend)
        .await
        .expect("run_pending_backend should succeed");
    assert_eq!(summary.applied, vec!["20261108_01_init".to_string()]);

    // 2. Ledger must be durably committed
    let count = backend
        .query_scalar("SELECT count(*) FROM rume_migrations WHERE id = '20261108_01_init'")
        .await
        .expect("query ledger")
        .expect("count present");
    assert_eq!(count, "1", "migration row must be durably committed");

    // 3. Named lock must be released so an independent session can acquire it immediately.
    // Use an independent MysqlBackend pool so MySQL's same-session recursive GET_LOCK cannot mask a leaked lock.
    let probe_backend = MysqlBackend::new(&config).expect("independent mysql probe backend");
    let is_free = probe_backend
        .query_scalar("SELECT IS_FREE_LOCK('rume_migrations_lock')")
        .await
        .expect("check IS_FREE_LOCK")
        .expect("IS_FREE_LOCK present");
    assert_eq!(
        is_free, "1",
        "named lock must be completely free after run_pending"
    );
    let tx_lock = probe_backend
        .begin_transaction()
        .await
        .expect("begin tx on independent session to probe lock");
    let lock_res = tx_lock
        .query_scalar("SELECT GET_LOCK('rume_migrations_lock', 0)")
        .await
        .expect("acquire named lock")
        .expect("lock result present");
    assert_eq!(
        lock_res, "1",
        "independent session must be able to acquire named lock after run_pending"
    );
    tx_lock
        .execute("SELECT RELEASE_LOCK('rume_migrations_lock')")
        .await
        .expect("release lock");
    tx_lock.commit().await.expect("commit probe tx");

    // 4. Rollback last migration
    let rolled_back = migrator
        .rollback_last_backend(&backend)
        .await
        .expect("rollback should succeed");
    assert_eq!(rolled_back, Some("20261108_01_init".to_string()));

    // 5. Ledger row must be removed
    let count_after = backend
        .query_scalar("SELECT count(*) FROM rume_migrations WHERE id = '20261108_01_init'")
        .await
        .expect("query ledger")
        .expect("count present");
    assert_eq!(
        count_after, "0",
        "migration row must be deleted on rollback"
    );

    // 6. Named lock must be released after rollback (verified from independent session)
    let is_free2 = probe_backend
        .query_scalar("SELECT IS_FREE_LOCK('rume_migrations_lock')")
        .await
        .expect("check IS_FREE_LOCK")
        .expect("IS_FREE_LOCK present");
    assert_eq!(
        is_free2, "1",
        "named lock must be completely free after rollback_last"
    );
    let tx_lock2 = probe_backend
        .begin_transaction()
        .await
        .expect("begin tx on independent session to probe lock");
    let lock_res2 = tx_lock2
        .query_scalar("SELECT GET_LOCK('rume_migrations_lock', 0)")
        .await
        .expect("acquire named lock")
        .expect("lock result present");
    assert_eq!(
        lock_res2, "1",
        "independent session must be able to acquire named lock after rollback_last"
    );
    tx_lock2
        .execute("SELECT RELEASE_LOCK('rume_migrations_lock')")
        .await
        .expect("release lock");
    tx_lock2.commit().await.expect("commit probe tx");
}

#[tokio::test]
async fn test_mysql_transaction_raw_no_param_and_error_classification() {
    use rume::persistence::PersistenceError;

    let (_node, backend, _config) = start_mysql_node().await;

    // 1. Raw no-param `execute` and `execute_params(..., &[])` on a transaction.
    let tx = backend.begin_transaction().await.expect("begin tx");
    tx.execute("CREATE TABLE mysql_raw_a (id INT PRIMARY KEY)")
        .await
        .expect("tx.execute raw DDL must succeed");
    tx.execute_params("INSERT INTO mysql_raw_a (id) VALUES (1)", &[])
        .await
        .expect("tx.execute_params with empty params must succeed");

    let count_a = tx
        .query_scalar("SELECT count(*) FROM mysql_raw_a")
        .await
        .expect("query mysql_raw_a")
        .expect("count present");
    assert_eq!(count_a, "1");
    tx.commit().await.expect("commit raw DDL tx");

    // 2. Killing the transaction's MySQL connection causes subsequent transaction calls to hit
    // an IO/EOF error, which must classify via `map_sqlx_error` as `ConnectionFailed` (not `QueryFailed`).
    let tx_drop = backend.begin_transaction().await.expect("begin tx_drop");
    let conn_id = tx_drop
        .query_scalar("SELECT CONNECTION_ID()")
        .await
        .expect("read CONNECTION_ID()")
        .expect("conn id present");
    let conn_num: u64 = conn_id.parse().expect("numeric connection id");

    backend
        .execute(&format!("KILL {conn_num}"))
        .await
        .expect("kill connection");

    let err_exec = tx_drop
        .execute("SELECT 1")
        .await
        .expect_err("execute on killed tx must fail");
    assert!(
        matches!(err_exec, PersistenceError::ConnectionFailed(_)),
        "expected ConnectionFailed from map_sqlx_error on killed tx execute, got: {err_exec:?}"
    );

    let err_exec_params = tx_drop
        .execute_params("SELECT ?", &rume::params![1_i64])
        .await
        .expect_err("execute_params on killed tx must fail");
    assert!(
        matches!(err_exec_params, PersistenceError::ConnectionFailed(_)),
        "expected ConnectionFailed from map_sqlx_error on killed tx execute_params, got: {err_exec_params:?}"
    );

    let err_qs = tx_drop
        .query_scalar("SELECT 1")
        .await
        .expect_err("query_scalar on killed tx must fail");
    assert!(
        matches!(err_qs, PersistenceError::ConnectionFailed(_)),
        "expected ConnectionFailed from map_sqlx_error on killed tx query_scalar, got: {err_qs:?}"
    );

    let err_fa = tx_drop
        .fetch_all("SELECT 1", &[])
        .await
        .expect_err("fetch_all on killed tx must fail");
    assert!(
        matches!(err_fa, PersistenceError::ConnectionFailed(_)),
        "expected ConnectionFailed from map_sqlx_error on killed tx fetch_all, got: {err_fa:?}"
    );

    let err_fo = tx_drop
        .fetch_optional("SELECT 1", &[])
        .await
        .expect_err("fetch_optional on killed tx must fail");
    assert!(
        matches!(err_fo, PersistenceError::ConnectionFailed(_)),
        "expected ConnectionFailed from map_sqlx_error on killed tx fetch_optional, got: {err_fo:?}"
    );
}
