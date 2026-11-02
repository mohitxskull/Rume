use axum::Json;
use axum::http::StatusCode;
use axum::routing::post;
use rume::persistence::{
    DatabaseBackend, DatabaseConfig, DatabasePool, DatabaseTransaction, MemoryDatabase, Migration,
    Migrator, PersistenceError, SqliteBackend,
};
use rume::prelude::*;

#[tokio::test]
async fn pool_lifecycle_and_capacity_limits() {
    let config = DatabaseConfig::new("memory://test")
        .with_min_connections(2)
        .with_max_connections(2);

    let db = MemoryDatabase::new();
    let pool = DatabasePool::new(db, config);

    let status = pool.status();
    assert_eq!(status.active_connections, 0);
    assert_eq!(status.total_connections, 2);
    assert_eq!(status.max_connections, 2);
    assert!(!status.is_closed);

    // Acquire lease 1
    let lease1 = pool.acquire_lease().expect("lease 1 should succeed");
    assert_eq!(pool.status().active_connections, 1);

    // Acquire lease 2
    let lease2 = pool.acquire_lease().expect("lease 2 should succeed");
    assert_eq!(pool.status().active_connections, 2);

    // Pool is now at max capacity; next acquisition must fail with PoolExhausted
    let lease3 = pool.acquire_lease();
    match lease3 {
        Err(PersistenceError::PoolExhausted { active, max }) => {
            assert_eq!(active, 2);
            assert_eq!(max, 2);
        }
        _ => panic!("expected PoolExhausted error"),
    }

    // Dropping lease 2 frees capacity
    drop(lease2);
    assert_eq!(pool.status().active_connections, 1);

    let lease2_retry = pool.acquire_lease().expect("should acquire after release");
    assert_eq!(pool.status().active_connections, 2);

    drop(lease1);
    drop(lease2_retry);
    assert_eq!(pool.status().active_connections, 0);

    // Closing the pool prevents further acquisitions
    pool.close();
    assert!(pool.status().is_closed);
    match pool.acquire_lease() {
        Err(PersistenceError::Closed) => {}
        _ => panic!("expected Closed error on closed pool"),
    }
}

#[tokio::test]
async fn explicit_transaction_commit_and_rollback() {
    let backend = SqliteBackend::in_memory().expect("sqlite in memory");
    backend
        .execute("CREATE TABLE users (id TEXT PRIMARY KEY, name TEXT NOT NULL);")
        .await
        .unwrap();
    let pool = DatabasePool::new(backend, DatabaseConfig::default());

    // 1. Successful transaction commits
    let res: Result<String, PersistenceError> = pool
        .transaction(|tx| async move {
            tx.execute("INSERT INTO users (id, name) VALUES ('user:1', 'alice');")
                .await?;
            tx.execute("INSERT INTO users (id, name) VALUES ('user:2', 'bob');")
                .await?;
            Ok("created".to_string())
        })
        .await;

    assert_eq!(res.unwrap(), "created");

    // Verify values were committed
    let val1 = pool
        .backend()
        .query_scalar("SELECT name FROM users WHERE id = 'user:1';")
        .await
        .unwrap();
    let val2 = pool
        .backend()
        .query_scalar("SELECT name FROM users WHERE id = 'user:2';")
        .await
        .unwrap();
    assert_eq!(val1.as_deref(), Some("alice"));
    assert_eq!(val2.as_deref(), Some("bob"));

    // 2. Failed transaction rolls back without mutating committed state
    let rollback_res: Result<(), PersistenceError> = pool
        .transaction(|tx| async move {
            tx.execute("INSERT INTO users (id, name) VALUES ('user:1', 'malory');")
                .await?;
            tx.execute("INSERT INTO users (id, name) VALUES ('user:3', 'charlie');")
                .await?;
            Err(PersistenceError::TransactionError("forced failure".into()))
        })
        .await;

    assert!(rollback_res.is_err());

    // user:1 should retain its original committed value 'alice', and user:3 must not exist
    let val1_after = pool
        .backend()
        .query_scalar("SELECT name FROM users WHERE id = 'user:1';")
        .await
        .unwrap();
    let val3_after = pool
        .backend()
        .query_scalar("SELECT name FROM users WHERE id = 'user:3';")
        .await
        .unwrap();
    assert_eq!(val1_after.as_deref(), Some("alice"));
    assert_eq!(val3_after, None);
}

#[tokio::test]
async fn migrations_apply_rollback_and_preflight_verification() {
    let backend = SqliteBackend::in_memory().expect("in-memory sqlite");

    let migrator = Migrator::new()
        .add_migration(
            Migration::from_sql(
                "001_create_accounts",
                "create accounts table",
                "CREATE TABLE accounts (id INTEGER PRIMARY KEY, balance INTEGER);",
            )
            .with_down("DROP TABLE accounts;"),
        )
        .add_migration(
            Migration::from_sql(
                "002_create_transactions",
                "create transactions table",
                "CREATE TABLE transactions (id INTEGER PRIMARY KEY, account_id INTEGER);",
            )
            .with_down("DROP TABLE transactions;"),
        );

    // Run pending migrations
    let summary = migrator.run_pending_backend(&backend).await.unwrap();
    assert_eq!(summary.applied.len(), 2);
    assert_eq!(summary.applied[0], "001_create_accounts");
    assert_eq!(summary.applied[1], "002_create_transactions");

    // Running pending again is idempotent
    let summary2 = migrator.run_pending_backend(&backend).await.unwrap();
    assert_eq!(summary2.applied.len(), 0);

    // Rollback last migration
    let rolled_back = migrator.rollback_last_backend(&backend).await.unwrap();
    assert_eq!(rolled_back.as_deref(), Some("002_create_transactions"));
}

#[tokio::test]
async fn irreversible_migration_rollback_fails_closed() {
    let backend = SqliteBackend::in_memory().expect("sqlite in memory");
    backend
        .execute("CREATE TABLE legacy_data (id INT);")
        .await
        .unwrap();

    let migrator = Migrator::new().add_migration(Migration::from_sql(
        "001_irreversible_drop",
        "drop legacy table",
        "DROP TABLE legacy_data;",
    ));

    migrator.run_pending_backend(&backend).await.unwrap();

    // Attempt rollback: must fail closed and NOT unmark the migration
    let res = migrator.rollback_last_backend(&backend).await;
    assert!(res.is_err());
    let err = res.unwrap_err();
    assert!(
        matches!(err, PersistenceError::IrreversibleMigration(ref id) if id == "001_irreversible_drop")
    );
}

#[tokio::test]
async fn health_check_reports_pool_status_and_failures() {
    let db = MemoryDatabase::new();
    let pool = DatabasePool::new(db.clone(), DatabaseConfig::default());
    let hc = pool.health_check("primary_db");

    // Initially healthy
    let result = hc.run().await;
    assert_eq!(result.status, HealthStatus::Ok);
    assert_eq!(result.name, "primary_db");
    assert_eq!(
        result.meta.get("active_connections").unwrap(),
        &serde_json::json!(0)
    );

    // Simulate database offline
    db.set_alive(false);
    let result_offline = hc.run().await;
    assert_eq!(result_offline.status, HealthStatus::Failed);
    assert!(
        result_offline
            .message
            .contains("database connection check failed")
    );

    // Restore database
    db.set_alive(true);
    let result_recovered = hc.run().await;
    assert_eq!(result_recovered.status, HealthStatus::Ok);

    // Pool closed
    pool.close();
    let result_closed = hc.run().await;
    assert_eq!(result_closed.status, HealthStatus::Failed);
    assert_eq!(result_closed.message, "database pool is closed");
}

#[derive(Clone)]
struct BillingModule {
    pool: DatabasePool<SqliteBackend>,
    migrator: Migrator,
}

impl BillingModule {
    fn new() -> Self {
        let backend = SqliteBackend::in_memory().expect("in-memory sqlite");
        let migrator = Migrator::new().add_migration(Migration::from_sql(
            "001_billing_accounts",
            "setup billing accounts",
            "CREATE TABLE IF NOT EXISTS accounts (id TEXT PRIMARY KEY, balance TEXT NOT NULL);",
        ));
        Self {
            pool: DatabasePool::new(backend, DatabaseConfig::default()),
            migrator,
        }
    }
}

impl Module for BillingModule {
    fn name(&self) -> &'static str {
        "billing"
    }

    fn on_start<'a>(
        &'a self,
        _ctx: &'a ModuleContext,
    ) -> rume::module::BoxFuture<'a, rume::module::ModuleHookResult> {
        Box::pin(async move {
            self.migrator
                .run_pending_backend(self.pool.backend())
                .await
                .map_err(|e| e.to_string())?;
            Ok(())
        })
    }

    fn on_shutdown<'a>(&'a self, _ctx: &'a ModuleContext) -> rume::module::BoxFuture<'a, ()> {
        Box::pin(async move {
            self.pool.close();
        })
    }

    fn register_routes(&self, router: rume::routing::Router) -> rume::routing::Router {
        let pool = self.pool.clone();

        router.route(
            "/billing/accounts",
            post(move |Json(payload): Json<serde_json::Value>| {
                let pool = pool.clone();
                async move {
                    let id = payload["id"].as_str().unwrap_or("unknown").to_string();
                    let balance = payload["balance"].as_str().unwrap_or("0").to_string();
                    let id_val = id.clone();

                    let res = pool
                        .transaction(move |tx| async move {
                            let stmt = format!(
                                "INSERT INTO accounts (id, balance) VALUES ('{id_val}', '{balance}');"
                            );
                            tx.execute(&stmt).await?;
                            Ok::<_, PersistenceError>(())
                        })
                        .await;

                    match res {
                        Ok(()) => (
                            StatusCode::CREATED,
                            Json(serde_json::json!({ "created": id })),
                        ),
                        Err(err) => (
                            StatusCode::INTERNAL_SERVER_ERROR,
                            Json(serde_json::json!({ "error": err.to_string() })),
                        ),
                    }
                }
            }),
        )
    }
}

#[tokio::test]
async fn module_persistence_end_to_end() {
    let billing = BillingModule::new();
    let test_app = Rume::new()
        .register_module(billing)
        .build_test_app()
        .await
        .expect("app should build");

    let res = test_app
        .post_json("/billing/accounts", r#"{"id":"acc_42","balance":"1500"}"#)
        .await;

    assert_eq!(res.status(), StatusCode::CREATED);
    let val: serde_json::Value = serde_json::from_str(&TestApp::body_text(res).await).unwrap();
    assert_eq!(val["created"], "acc_42");
}

#[tokio::test]
async fn pool_acquire_timeout_and_graceful_drain() {
    use std::time::Duration;

    let config = DatabaseConfig::new("memory://test")
        .with_min_connections(1)
        .with_max_connections(1)
        .with_connect_timeout(1);

    let pool = DatabasePool::new(MemoryDatabase::new(), config);

    // Hold the single available connection permit
    let lease1 = pool
        .acquire_lease_timeout(Duration::from_millis(50))
        .await
        .expect("first lease should succeed");

    // Second acquisition times out waiting for permit
    let timeout_res = pool.acquire_lease_timeout(Duration::from_millis(20)).await;
    assert!(matches!(timeout_res, Err(PersistenceError::Timeout)));

    // Spawn async release after 30ms
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(30)).await;
        drop(lease1);
    });

    // Third acquisition waits and successfully acquires once lease1 drops
    let lease2 = pool
        .acquire_lease_timeout(Duration::from_millis(150))
        .await
        .expect("should acquire after permit freed");

    // Now test close_and_drain:
    // If lease2 is held, close_and_drain times out if timeout is too short
    let drained_early = pool.close_and_drain(Duration::from_millis(10)).await;
    assert!(!drained_early, "should not drain while lease is active");

    drop(lease2);

    let drained_clean = pool.close_and_drain(Duration::from_millis(50)).await;
    assert!(drained_clean, "should cleanly drain once lease is released");
    assert!(pool.status().is_closed);
}
