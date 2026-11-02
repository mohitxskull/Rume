//! Database migration definitions, runner, and preflight verification.

use std::sync::Arc;

use crate::persistence::backend::{BoxFuture, DatabaseBackend};
use crate::persistence::error::PersistenceError;

/// Function signature for dynamic migration execution.
pub type MigrationFn = Arc<
    dyn Fn(Arc<dyn std::any::Any + Send + Sync>) -> BoxFuture<'static, Result<(), PersistenceError>>
        + Send
        + Sync,
>;

/// A single versioned database migration step.
#[derive(Clone)]
pub struct Migration {
    /// Unique identifier for this migration (e.g., "20261102_001_create_users").
    pub id: String,
    /// Human-readable description of changes in this migration.
    pub description: String,
    /// Up statement to execute.
    pub up_sql: Option<String>,
    /// Down statement to execute on rollback.
    pub down_sql: Option<String>,
}

impl std::fmt::Debug for Migration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Migration")
            .field("id", &self.id)
            .field("description", &self.description)
            .field("up_sql", &self.up_sql)
            .field("down_sql", &self.down_sql)
            .finish()
    }
}

impl Migration {
    /// Creates a new migration with an up statement.
    pub fn from_sql(
        id: impl Into<String>,
        description: impl Into<String>,
        up_sql: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            description: description.into(),
            up_sql: Some(up_sql.into()),
            down_sql: None,
        }
    }

    /// Attaches a down statement for rollback support.
    pub fn with_down(mut self, down_sql: impl Into<String>) -> Self {
        self.down_sql = Some(down_sql.into());
        self
    }
}

/// Summary report of executed migrations.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MigrationSummary {
    /// Identifiers of applied migrations in order of execution.
    pub applied: Vec<String>,
}

/// Migration registry and execution runner.
#[derive(Debug, Default, Clone)]
pub struct Migrator {
    migrations: Vec<Migration>,
}

impl Migrator {
    /// Creates an empty migrator registry.
    pub fn new() -> Self {
        Self {
            migrations: Vec::new(),
        }
    }

    /// Registers a migration.
    pub fn add_migration(mut self, migration: Migration) -> Self {
        self.migrations.push(migration);
        self
    }

    /// Returns registered migrations.
    pub fn migrations(&self) -> &[Migration] {
        &self.migrations
    }

    /// Finds all registered migrations that have not yet been applied to the given database backend.
    pub async fn pending_migrations<B: DatabaseBackend>(
        &self,
        backend: &B,
    ) -> Result<Vec<String>, PersistenceError> {
        backend
            .execute(
                "CREATE TABLE IF NOT EXISTS rume_migrations (id VARCHAR(255) PRIMARY KEY, applied_at BIGINT NOT NULL)",
            )
            .await?;

        let mut pending = Vec::new();
        for migration in &self.migrations {
            let check_sql = format!(
                "SELECT id FROM rume_migrations WHERE id = '{}'",
                migration.id.replace('\'', "''")
            );
            let exists = backend.query_scalar(&check_sql).await?;
            if exists.is_none() {
                pending.push(migration.id.clone());
            }
        }
        Ok(pending)
    }

    /// Executes all unapplied pending migrations in order on any [`DatabaseBackend`].
    pub async fn run_pending<B: DatabaseBackend>(
        &self,
        backend: &B,
    ) -> Result<MigrationSummary, PersistenceError> {
        self.run_pending_backend(backend).await
    }

    /// Rolls back the most recently applied migration on any [`DatabaseBackend`].
    ///
    /// Fails closed with [`PersistenceError::IrreversibleMigration`] if the target
    /// migration lacks a `down_sql` rollback script, preserving executed state.
    pub async fn rollback_last<B: DatabaseBackend>(
        &self,
        backend: &B,
    ) -> Result<Option<String>, PersistenceError> {
        self.rollback_last_backend(backend).await
    }

    /// Preflight verification on any [`DatabaseBackend`]: fails closed if any registered migrations have not been applied.
    pub async fn verify_no_pending<B: DatabaseBackend>(
        &self,
        backend: &B,
    ) -> Result<(), PersistenceError> {
        self.verify_no_pending_backend(backend).await
    }

    /// Executes all unapplied pending migrations in order on any [`DatabaseBackend`].
    pub async fn run_pending_backend<B: DatabaseBackend>(
        &self,
        backend: &B,
    ) -> Result<MigrationSummary, PersistenceError> {
        backend
            .execute(
                "CREATE TABLE IF NOT EXISTS rume_migrations (id VARCHAR(255) PRIMARY KEY, applied_at BIGINT NOT NULL)",
            )
            .await?;

        let mut applied = Vec::new();
        for migration in &self.migrations {
            let check_sql = format!(
                "SELECT id FROM rume_migrations WHERE id = '{}'",
                migration.id.replace('\'', "''")
            );
            let exists = backend.query_scalar(&check_sql).await?;
            if exists.is_none() {
                if let Some(sql) = &migration.up_sql {
                    backend
                        .execute(sql)
                        .await
                        .map_err(|err| PersistenceError::MigrationError {
                            migration: migration.id.clone(),
                            message: err.to_string(),
                        })?;
                }
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                let insert_sql = format!(
                    "INSERT INTO rume_migrations (id, applied_at) VALUES ('{}', {})",
                    migration.id.replace('\'', "''"),
                    now
                );
                backend.execute(&insert_sql).await?;
                applied.push(migration.id.clone());
            }
        }

        Ok(MigrationSummary { applied })
    }

    /// Rolls back the most recently applied migration on any [`DatabaseBackend`].
    pub async fn rollback_last_backend<B: DatabaseBackend>(
        &self,
        backend: &B,
    ) -> Result<Option<String>, PersistenceError> {
        backend
            .execute(
                "CREATE TABLE IF NOT EXISTS rume_migrations (id VARCHAR(255) PRIMARY KEY, applied_at BIGINT NOT NULL)",
            )
            .await?;

        for migration in self.migrations.iter().rev() {
            let check_sql = format!(
                "SELECT id FROM rume_migrations WHERE id = '{}'",
                migration.id.replace('\'', "''")
            );
            let exists = backend.query_scalar(&check_sql).await?;
            if exists.is_some() {
                let Some(sql) = &migration.down_sql else {
                    return Err(PersistenceError::IrreversibleMigration(
                        migration.id.clone(),
                    ));
                };

                backend
                    .execute(sql)
                    .await
                    .map_err(|err| PersistenceError::MigrationError {
                        migration: migration.id.clone(),
                        message: err.to_string(),
                    })?;

                let delete_sql = format!(
                    "DELETE FROM rume_migrations WHERE id = '{}'",
                    migration.id.replace('\'', "''")
                );
                backend.execute(&delete_sql).await?;
                return Ok(Some(migration.id.clone()));
            }
        }

        Ok(None)
    }

    /// Preflight verification on any [`DatabaseBackend`]: fails closed if any registered migrations have not been applied.
    pub async fn verify_no_pending_backend<B: DatabaseBackend>(
        &self,
        backend: &B,
    ) -> Result<(), PersistenceError> {
        backend
            .execute(
                "CREATE TABLE IF NOT EXISTS rume_migrations (id VARCHAR(255) PRIMARY KEY, applied_at BIGINT NOT NULL)",
            )
            .await?;

        let mut pending = Vec::new();
        for migration in &self.migrations {
            let check_sql = format!(
                "SELECT id FROM rume_migrations WHERE id = '{}'",
                migration.id.replace('\'', "''")
            );
            let exists = backend.query_scalar(&check_sql).await?;
            if exists.is_none() {
                pending.push(migration.id.clone());
            }
        }

        if pending.is_empty() {
            Ok(())
        } else {
            Err(PersistenceError::PendingMigrations(pending))
        }
    }
}
