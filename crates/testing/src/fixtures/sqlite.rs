use super::TestTransactionFixture;
use super::*;

/// An isolated SQLite database backed by a temporary file on disk, automatically deleted on Drop.
#[cfg(feature = "sqlite")]
pub struct SqliteTestFixture {
    path: PathBuf,
    pool: DatabasePool<rume_data::persistence::SqliteBackend>,
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

        let backend = rume_data::persistence::SqliteBackend::open(&path_str).await?;
        let config = DatabaseConfig::new(format!("sqlite://{path_str}"));
        let pool = DatabasePool::new(backend, config);

        Ok(Self {
            path,
            pool,
            closed: false,
        })
    }

    /// Returns a reference to the database pool.
    pub fn pool(&self) -> &DatabasePool<rume_data::persistence::SqliteBackend> {
        &self.pool
    }

    /// Returns the file path of the temporary SQLite database.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Begins an isolated transaction wrapped in a [`TestTransactionFixture`].
    pub async fn begin_test_tx(
        &self,
    ) -> Result<TestTransactionFixture<rume_data::persistence::SqliteTransaction>, PersistenceError>
    {
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
            std::fs::remove_file(&path)
                .map_err(|e| PersistenceError::ConnectionFailed(e.to_string()))?;
        }
        Ok(())
    }
}

#[cfg(feature = "sqlite")]
impl std::ops::Deref for SqliteTestFixture {
    type Target = DatabasePool<rume_data::persistence::SqliteBackend>;
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
