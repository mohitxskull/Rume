//! Isolated database and transaction fixtures for test isolation.

#[cfg(feature = "sqlite")]
use std::path::{Path, PathBuf};
#[cfg(feature = "sqlite")]
use crate::persistence::DatabaseConfig;
#[cfg(any(feature = "sqlite", feature = "postgres"))]
use crate::persistence::{DatabaseBackend, DatabasePool};
use crate::persistence::{DatabaseTransaction, PersistenceError};

/// An RAII transaction guard that automatically rolls back the transaction on Drop
/// unless explicitly committed.
pub struct TestTransactionFixture<T: DatabaseTransaction + 'static> {
    tx: Option<T>,
    committed: bool,
}

impl<T: DatabaseTransaction + 'static> TestTransactionFixture<T> {
    /// Creates a new fixture wrapping an active database transaction.
    pub fn new(tx: T) -> Self {
        Self {
            tx: Some(tx),
            committed: false,
        }
    }

    /// Returns a reference to the active transaction.
    pub fn tx(&self) -> &T {
        self.tx.as_ref().expect("transaction active")
    }

    /// Atomically commits the transaction, consuming the fixture and preventing rollback on Drop.
    pub async fn commit(mut self) -> Result<(), PersistenceError> {
        if let Some(tx) = self.tx.take() {
            self.committed = true;
            tx.commit().await?;
        }
        Ok(())
    }

    /// Discards and rolls back the transaction explicitly, awaiting completion.
    pub async fn rollback(mut self) -> Result<(), PersistenceError> {
        if let Some(tx) = self.tx.take() {
            tx.rollback().await?;
        }
        Ok(())
    }

    /// Discards and rolls back the transaction explicitly, awaiting completion.
    pub async fn close(self) -> Result<(), PersistenceError> {
        self.rollback().await
    }
}

impl<T: DatabaseTransaction + 'static> std::ops::Deref for TestTransactionFixture<T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        self.tx()
    }
}

impl<T: DatabaseTransaction + 'static> std::fmt::Debug for TestTransactionFixture<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TestTransactionFixture")
            .field("committed", &self.committed)
            .finish()
    }
}

impl<T: DatabaseTransaction + 'static> Drop for TestTransactionFixture<T> {
    fn drop(&mut self) {
        if !self.committed
            && let Some(tx) = self.tx.take()
            && let Ok(handle) = tokio::runtime::Handle::try_current()
        {
            handle.spawn(async move {
                let _ = tx.rollback().await;
            });
        }
    }
}

/// An isolated SQLite database backed by a temporary file on disk, automatically deleted on Drop.
#[cfg(feature = "sqlite")]
pub struct SqliteTestFixture {
    path: PathBuf,
    pool: DatabasePool<crate::persistence::SqliteBackend>,
    closed: bool,
}

#[cfg(feature = "sqlite")]
impl SqliteTestFixture {
    /// Creates a unique temporary SQLite database file and initializes a connection pool.
    pub async fn create() -> Result<Self, PersistenceError> {
        let temp_dir = std::env::temp_dir();
        let db_name = format!("rume_test_{}.db", uuid::Uuid::new_v4().simple());
        let path = temp_dir.join(db_name);
        let path_str = path.to_string_lossy().to_string();

        let backend = crate::persistence::SqliteBackend::open(&path_str).await?;
        let config = DatabaseConfig::new(format!("sqlite://{path_str}"));
        let pool = DatabasePool::new(backend, config);

        Ok(Self {
            path,
            pool,
            closed: false,
        })
    }

    /// Returns a reference to the database pool.
    pub fn pool(&self) -> &DatabasePool<crate::persistence::SqliteBackend> {
        &self.pool
    }

    /// Returns the file path of the temporary SQLite database.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Begins an isolated transaction wrapped in a [`TestTransactionFixture`].
    pub async fn begin_test_tx(&self) -> Result<TestTransactionFixture<crate::persistence::SqliteTransaction>, PersistenceError> {
        let tx = self.pool.backend().begin_transaction().await?;
        Ok(TestTransactionFixture::new(tx))
    }

    /// Explicitly drains the pool and deletes temporary files, returning any errors to the test runner.
    pub async fn close(mut self) -> Result<(), PersistenceError> {
        self.closed = true;
        // 1. Drain and close connection pool
        self.pool.close_async().await;
        // 2. Remove files deterministically
        let path = self.path.clone();
        let wal = PathBuf::from(format!("{}-wal", path.display()));
        let shm = PathBuf::from(format!("{}-shm", path.display()));
        if wal.exists() {
            let _ = std::fs::remove_file(wal);
        }
        if shm.exists() {
            let _ = std::fs::remove_file(shm);
        }
        if path.exists() {
            std::fs::remove_file(&path).map_err(|e| PersistenceError::ConnectionFailed(e.to_string()))?;
        }
        Ok(())
    }
}

#[cfg(feature = "sqlite")]
impl std::ops::Deref for SqliteTestFixture {
    type Target = DatabasePool<crate::persistence::SqliteBackend>;
    fn deref(&self) -> &Self::Target {
        &self.pool
    }
}

#[cfg(feature = "sqlite")]
impl std::fmt::Debug for SqliteTestFixture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqliteTestFixture")
            .field("path", &self.path)
            .field("closed", &self.closed)
            .finish()
    }
}

#[cfg(feature = "sqlite")]
impl Drop for SqliteTestFixture {
    fn drop(&mut self) {
        if !self.closed {
            let path = self.path.clone();
            let _ = std::fs::remove_file(format!("{}-wal", path.display()));
            let _ = std::fs::remove_file(format!("{}-shm", path.display()));
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// An isolated PostgreSQL test fixture utilizing a unique dedicated schema per test.
#[cfg(feature = "postgres")]
pub struct PostgresSchemaFixture {
    schema_name: String,
    pool: DatabasePool<crate::persistence::PostgresBackend>,
    admin_pool: Option<DatabasePool<crate::persistence::PostgresBackend>>,
    closed: bool,
}

#[cfg(feature = "postgres")]
impl PostgresSchemaFixture {
    /// Creates a unique schema in the target PostgreSQL instance and returns a pool scoped to that schema.
    pub async fn create(admin_pool: &DatabasePool<crate::persistence::PostgresBackend>) -> Result<Self, PersistenceError> {
        let schema_name = format!("test_{}", uuid::Uuid::new_v4().simple());

        admin_pool
            .backend()
            .execute(&format!("CREATE SCHEMA \"{schema_name}\""))
            .await?;

        let base_url = &admin_pool.config().url;
        let scoped_url = if base_url.contains('?') {
            format!("{base_url}&options=-csearch_path%3D{schema_name}")
        } else {
            format!("{base_url}?options=-csearch_path%3D{schema_name}")
        };

        let mut config = admin_pool.config().clone();
        config.url = scoped_url;
        let backend = crate::persistence::PostgresBackend::connect(&config).await?;
        let pool = DatabasePool::new(backend, config);

        Ok(Self {
            schema_name,
            pool,
            admin_pool: Some(admin_pool.clone()),
            closed: false,
        })
    }

    /// Returns the schema name created for this test run.
    pub fn schema_name(&self) -> &str {
        &self.schema_name
    }

    /// Returns a reference to the pool scoped to this test's isolated schema.
    pub fn pool(&self) -> &DatabasePool<crate::persistence::PostgresBackend> {
        &self.pool
    }

    /// Begins an isolated transaction wrapped in a [`TestTransactionFixture`].
    pub async fn begin_test_tx(&self) -> Result<TestTransactionFixture<crate::persistence::PostgresTransaction>, PersistenceError> {
        let tx = self.pool.backend().begin_transaction().await?;
        Ok(TestTransactionFixture::new(tx))
    }

    /// Explicitly drains the pool and drops the schema, returning any cleanup errors to the test runner.
    pub async fn close(mut self) -> Result<(), PersistenceError> {
        self.closed = true;
        self.pool.close_async().await;
        if let Some(admin) = self.admin_pool.take() {
            admin
                .backend()
                .execute(&format!("DROP SCHEMA IF EXISTS \"{}\" CASCADE", self.schema_name))
                .await?;
        }
        Ok(())
    }
}

#[cfg(feature = "postgres")]
impl std::ops::Deref for PostgresSchemaFixture {
    type Target = DatabasePool<crate::persistence::PostgresBackend>;
    fn deref(&self) -> &Self::Target {
        &self.pool
    }
}

#[cfg(feature = "postgres")]
impl std::fmt::Debug for PostgresSchemaFixture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PostgresSchemaFixture")
            .field("schema_name", &self.schema_name)
            .field("closed", &self.closed)
            .finish()
    }
}

#[cfg(feature = "postgres")]
impl Drop for PostgresSchemaFixture {
    fn drop(&mut self) {
        if !self.closed
            && let Some(admin) = self.admin_pool.take()
        {
            let schema = self.schema_name.clone();
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move {
                    let _ = admin
                        .backend()
                        .execute(&format!("DROP SCHEMA IF EXISTS \"{schema}\" CASCADE"))
                        .await;
                });
            }
        }
    }
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_sqlite_test_fixture_creates_and_cleans_up() {
        let fixture = SqliteTestFixture::create().await.expect("fixture creates");
        let path = fixture.path().to_path_buf();
        assert!(path.exists(), "database file must exist on disk");

        fixture.pool().backend().execute("CREATE TABLE users (id INT, name TEXT)").await.expect("execute");
        fixture.pool().backend().execute("INSERT INTO users VALUES (1, 'Alice')").await.expect("insert");

        let count: Option<String> = fixture.pool().backend().query_scalar("SELECT COUNT(*) FROM users").await.expect("count");
        assert_eq!(count.as_deref(), Some("1"));

        // Drop fixture
        drop(fixture);

        // Path must be removed from disk
        assert!(!path.exists(), "database file must be cleaned up on drop");
    }

    #[tokio::test]
    async fn test_transaction_fixture_auto_rollback_on_drop() {
        let fixture = SqliteTestFixture::create().await.expect("fixture creates");
        fixture.pool().backend().execute("CREATE TABLE items (val INT)").await.expect("create table");

        // Transaction 1: inserted but dropped without commit
        {
            let tx = fixture.begin_test_tx().await.expect("begin tx");
            tx.execute("INSERT INTO items VALUES (999)").await.expect("insert in tx");
            // drop tx
        }

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let count: Option<String> = fixture.pool().backend().query_scalar("SELECT COUNT(*) FROM items").await.expect("count");
        assert_eq!(count.as_deref(), Some("0"), "uncommitted transaction must be rolled back on drop");

        // Transaction 2: committed
        {
            let tx = fixture.begin_test_tx().await.expect("begin tx");
            tx.execute("INSERT INTO items VALUES (123)").await.expect("insert in tx");
            tx.commit().await.expect("commit");
        }

        let count2: Option<String> = fixture.pool().backend().query_scalar("SELECT COUNT(*) FROM items").await.expect("count");
        assert_eq!(count2.as_deref(), Some("1"), "committed transaction must persist");
        fixture.close().await.expect("fixture close");
    }

    #[tokio::test]
    async fn test_sqlite_test_fixture_explicit_close_and_cleanup() {
        let fixture = SqliteTestFixture::create().await.expect("fixture creates");
        let path = fixture.path().to_path_buf();
        assert!(path.exists());

        fixture.pool().backend().execute("CREATE TABLE t (id INT)").await.expect("execute");
        fixture.close().await.expect("explicit close should succeed");

        assert!(!path.exists(), "database file must be cleaned up on close");
        assert!(!PathBuf::from(format!("{}-wal", path.display())).exists());
        assert!(!PathBuf::from(format!("{}-shm", path.display())).exists());
    }

    #[tokio::test]
    async fn test_transaction_fixture_explicit_rollback() {
        let fixture = SqliteTestFixture::create().await.expect("fixture creates");
        fixture.pool().backend().execute("CREATE TABLE t (v INT)").await.expect("create");

        let tx = fixture.begin_test_tx().await.expect("tx");
        tx.execute("INSERT INTO t VALUES (42)").await.expect("insert");
        tx.rollback().await.expect("explicit rollback should succeed");

        let count: Option<String> = fixture.pool().backend().query_scalar("SELECT COUNT(*) FROM t").await.expect("count");
        assert_eq!(count.as_deref(), Some("0"));

        fixture.close().await.expect("close");
    }

    #[tokio::test]
    async fn test_parallel_fixtures_deterministic_isolation() {
        let mut handles = Vec::new();
        for i in 0..5 {
            handles.push(tokio::spawn(async move {
                let fixture = SqliteTestFixture::create().await.expect("create");
                fixture.pool().backend().execute("CREATE TABLE p (v INT)").await.expect("create");
                fixture.pool().backend().execute(&format!("INSERT INTO p VALUES ({i})")).await.expect("insert");
                let count: Option<String> = fixture.pool().backend().query_scalar("SELECT COUNT(*) FROM p").await.expect("count");
                assert_eq!(count.as_deref(), Some("1"));
                let path = fixture.path().to_path_buf();
                fixture.close().await.expect("close");
                assert!(!path.exists());
            }));
        }

        for h in handles {
            h.await.expect("task completes successfully");
        }
    }
}
