#![cfg(feature = "sqlite")]

use std::sync::Arc;

use rume::outbox::{OutboxMessage, OutboxRelay, OutboxRepository, TransactionalOutbox};
use rume::persistence::{
    DatabaseBackend, DatabaseConfig, DatabasePool, DatabaseTransaction, Migration, Migrator,
    SqliteBackend,
};

#[tokio::test]
async fn test_sqlite_unified_database_conformance_suite() {
    let fixture = rume::testing::SqliteTestFixture::create()
        .await
        .expect("create sqlite fixture");
    rume::testing::verify_database_conformance(fixture.pool(), "sqlite")
        .await
        .expect("sqlite database conformance suite must pass");
    fixture.close().await.expect("close sqlite fixture");
}

#[tokio::test]
async fn sqlite_pool_lifecycle_and_ping() {
    let backend = SqliteBackend::in_memory()
        .await
        .expect("in-memory sqlite should initialize");
    let pool = DatabasePool::new(
        backend,
        DatabaseConfig::new("sqlite::memory:")
            .with_min_connections(2)
            .with_max_connections(5),
    );

    pool.backend().ping().await.expect("ping should succeed");
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    let mut status = pool.status();
    while status.active_connections != 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "pool failed to return connection to idle within 2s (active_connections={})",
            status.active_connections
        );
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        status = pool.status();
    }

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
    let res2 = pool2.apply_auto_migrations(&migrator).await.expect("no-op");
    assert!(res2.is_none());

    // When auto_migrate_core is called:
    let backend3 = SqliteBackend::in_memory().await.expect("in-memory sqlite");
    let config3 = DatabaseConfig::default().with_auto_migrate(true);
    let pool3 = DatabasePool::new(backend3, config3);
    let res3 = pool3
        .apply_auto_migrations_core()
        .await
        .expect("core auto migrate");
    assert!(res3.is_some());
    assert_eq!(res3.unwrap().applied.len(), 1);
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

#[tokio::test]
async fn sqlite_parameterized_transaction_and_safe_values() {
    let backend = SqliteBackend::in_memory().await.expect("in-memory sqlite");
    backend
        .execute("CREATE TABLE products (id TEXT PRIMARY KEY, name TEXT NOT NULL, price REAL, description TEXT);")
        .await
        .expect("table should create");

    let tx = backend.begin_transaction().await.expect("begin tx");

    // Test quotes, malicious SQL injection attempts, Unicode, and nulls
    let malicious_name = "Robert'); DROP TABLE products; --";
    let unicode_desc = "🦀 High Performance & Safe 🚀";
    let sql = "INSERT INTO products (id, name, price, description) VALUES (?, ?, ?, ?);";

    let rows = tx
        .execute_params(
            sql,
            &rume::params!["prod-1", malicious_name, 99.95, unicode_desc,],
        )
        .await
        .expect("parameterized insert should succeed");
    assert_eq!(rows, 1);

    // Insert product with null description
    let rows_null = tx
        .execute_params(
            sql,
            &rume::params!["prod-2", "Standard Widget", 19.99, None as Option<String>,],
        )
        .await
        .expect("insert with null should succeed");
    assert_eq!(rows_null, 1);

    // Query scalar with parameter inside transaction
    let queried_name = tx
        .query_scalar_params(
            "SELECT name FROM products WHERE id = ?",
            &rume::params!["prod-1"],
        )
        .await
        .expect("query scalar with param")
        .expect("product name found");
    assert_eq!(queried_name, malicious_name);

    tx.commit().await.expect("commit should succeed");

    // Verify products table still exists and contains both records
    let total_count = backend
        .query_scalar("SELECT count(*) FROM products;")
        .await
        .expect("count query")
        .expect("count present");
    assert_eq!(total_count, "2");

    // Test raw_tx escape hatch
    let tx_native = backend.begin_transaction().await.expect("begin native tx");
    let mut guard = tx_native.raw_tx().lock().await;
    let active_tx = guard.as_mut().expect("active tx present");
    sqlx::query("INSERT INTO products (id, name, price, description) VALUES (?, ?, ?, ?)")
        .bind("prod-3")
        .bind("Native SQLx Product")
        .bind(42.0)
        .bind("Direct driver access")
        .execute(&mut **active_tx)
        .await
        .expect("native driver execute");
    drop(guard);
    tx_native.commit().await.expect("commit native tx");

    let count_after_native = backend
        .query_scalar("SELECT count(*) FROM products;")
        .await
        .expect("count query")
        .expect("count present");
    assert_eq!(count_after_native, "3");
}

#[tokio::test]
async fn sqlite_test_harness_integration() {
    use rume::testing::RumeTestHarness;

    let router = axum::Router::new().route(
        "/status",
        axum::routing::get(|| async { "all-systems-nominal" }),
    );
    let harness = RumeTestHarness::in_memory()
        .await
        .expect("in-memory harness init")
        .with_router(router);

    let resp = harness.get("/status").await;
    assert_eq!(resp.status(), axum::http::StatusCode::OK);
    assert_eq!(
        rume::testing::TestApp::body_text(resp).await,
        "all-systems-nominal"
    );

    harness
        .execute("CREATE TABLE inventory (id INTEGER PRIMARY KEY, sku TEXT NOT NULL);")
        .await
        .expect("table creation succeeds");

    harness
        .execute("INSERT INTO inventory (id, sku) VALUES (1, 'SKU-001');")
        .await
        .expect("insert succeeds");

    let sku = harness
        .query_scalar("SELECT sku FROM inventory WHERE id = 1;")
        .await
        .expect("query succeeds");
    assert_eq!(sku, Some("SKU-001".into()));
}

#[tokio::test]
async fn sqlite_transaction_raw_no_param_execute_and_error_mapping() {
    use rume::persistence::PersistenceError;

    let backend = SqliteBackend::in_memory().await.expect("in-memory sqlite");
    let tx = backend.begin_transaction().await.expect("begin tx");

    // Multi-statement raw SQL via both `execute` and `execute_params(..., &[])`.
    tx.execute("CREATE TABLE raw_e1 (id INTEGER PRIMARY KEY); INSERT INTO raw_e1 (id) VALUES (1);")
        .await
        .expect("tx.execute raw multi-statement must succeed");

    tx.execute_params(
        "CREATE TABLE raw_p1 (id INTEGER PRIMARY KEY); CREATE TABLE raw_p2 (id INTEGER PRIMARY KEY);",
        &[],
    )
    .await
    .expect("tx.execute_params with empty params must run raw multi-statement SQL");

    let e1_count = tx
        .query_scalar("SELECT count(*) FROM raw_e1;")
        .await
        .expect("query raw_e1")
        .expect("count present");
    assert_eq!(e1_count, "1");

    // Both tables from the empty-params `execute_params` call must exist.
    tx.execute_params(
        "INSERT INTO raw_p2 (id) VALUES (?);",
        &rume::params![42_i64],
    )
    .await
    .expect("raw_p2 created by multi-statement execute_params(..., &[]) must exist");

    // Trigger constraint violation via `execute` and `execute_params`.
    let dup_raw = tx
        .execute("INSERT INTO raw_e1 (id) VALUES (1);")
        .await
        .expect_err("duplicate PK via execute must fail");
    assert!(
        matches!(dup_raw, PersistenceError::QueryFailed(_)),
        "expected QueryFailed on duplicate PK, got: {dup_raw:?}"
    );

    let dup_params = tx
        .execute_params(
            "INSERT INTO raw_p2 (id) VALUES (?);",
            &rume::params![42_i64],
        )
        .await
        .expect_err("duplicate PK via execute_params must fail");
    assert!(
        matches!(dup_params, PersistenceError::QueryFailed(_)),
        "expected QueryFailed on duplicate PK, got: {dup_params:?}"
    );

    // Invalid SQL across query_scalar, fetch_all, fetch_optional maps to QueryFailed.
    let qs_err = tx
        .query_scalar("SELECT * FROM no_such_table;")
        .await
        .expect_err("query_scalar on missing table must fail");
    assert!(matches!(qs_err, PersistenceError::QueryFailed(_)));

    let fa_err = tx
        .fetch_all("SELECT * FROM no_such_table;", &[])
        .await
        .expect_err("fetch_all on missing table must fail");
    assert!(matches!(fa_err, PersistenceError::QueryFailed(_)));

    let fo_err = tx
        .fetch_optional("SELECT * FROM no_such_table;", &[])
        .await
        .expect_err("fetch_optional on missing table must fail");
    assert!(matches!(fo_err, PersistenceError::QueryFailed(_)));

    tx.rollback().await.expect("rollback");

    // After resolution, transaction methods return TransactionError.
    let after_err = tx
        .execute("SELECT 1;")
        .await
        .expect_err("resolved tx must fail");
    assert!(matches!(after_err, PersistenceError::TransactionError(_)));
}
