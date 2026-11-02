//! Persistence error types.

use thiserror::Error;

/// Errors that can occur during database operations, connection management, or migrations.
#[derive(Debug, Error)]
pub enum PersistenceError {
    /// Failed to establish a database connection.
    #[error("connection failed: {0}")]
    ConnectionFailed(String),

    /// Connection pool is exhausted and cannot serve the request within timeout.
    #[error("connection pool exhausted: active={active}, max={max}")]
    PoolExhausted { active: u32, max: u32 },

    /// An explicit transaction failed during execution.
    #[error("transaction error: {0}")]
    TransactionError(String),

    /// A transaction was rolled back due to an error.
    #[error("transaction rolled back: {0}")]
    TransactionRolledBack(String),

    /// A database migration failed.
    #[error("migration '{migration}' failed: {message}")]
    MigrationError { migration: String, message: String },

    /// Pending migrations exist that have not been applied.
    #[error("unapplied pending migrations detected: {0:?}")]
    PendingMigrations(Vec<String>),

    /// Attempted to rollback a migration that has no down migration script.
    #[error("cannot rollback irreversible migration '{0}': missing down_sql")]
    IrreversibleMigration(String),

    /// Database pool or connection is already closed.
    #[error("database is closed")]
    Closed,

    /// Connection acquisition or operation timed out.
    #[error("database operation timed out")]
    Timeout,

    /// Statement or query execution failed.
    #[error("query execution failed: {0}")]
    QueryFailed(String),
}
