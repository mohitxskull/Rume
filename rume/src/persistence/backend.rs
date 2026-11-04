//! Database backend and transaction abstractions.

use std::fmt::Debug;
use std::future::Future;
use std::pin::Pin;

use crate::persistence::error::PersistenceError;

/// Pinned boxed future type alias for async database traits.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Generic interface implemented by database drivers and storage backends.
pub trait DatabaseBackend: Debug + Send + Sync + 'static {
    /// Type of transaction produced by this backend.
    type Transaction: DatabaseTransaction + Clone + 'static;

    /// Verifies connectivity to the database.
    fn ping(&self) -> BoxFuture<'_, Result<(), PersistenceError>>;

    /// Executes a SQL or DDL statement directly, returning the number of affected rows.
    fn execute(&self, statement: &str) -> BoxFuture<'_, Result<u64, PersistenceError>>;

    /// Executes a scalar query returning an optional string value.
    fn query_scalar(&self, query: &str) -> BoxFuture<'_, Result<Option<String>, PersistenceError>>;

    /// Begins a new transaction.
    fn begin_transaction(&self) -> BoxFuture<'_, Result<Self::Transaction, PersistenceError>>;

    /// Returns the current number of open physical connections managed by the backend driver.
    fn pool_size(&self) -> u32 {
        0
    }

    /// Returns the number of idle connections currently available in the backend pool.
    fn pool_idle(&self) -> u32 {
        0
    }

    /// Returns whether the underlying driver pool has been closed.
    fn is_closed(&self) -> bool {
        false
    }

    /// Closes the physical database connection pool.
    fn close(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {})
    }

    /// Returns the database dialect (e.g. "postgres", "mysql", "sqlite", "default").
    fn dialect(&self) -> &'static str {
        "default"
    }

    /// Records an audit log event using parameterized query binding.
    #[allow(clippy::too_many_arguments)]
    fn record_audit_log(
        &self,
        id: &str,
        action: &str,
        resource: &str,
        status: &str,
        actor_id: Option<&str>,
        target_id: Option<&str>,
        ip_address: Option<&str>,
        metadata: &str,
        created_at: i64,
    ) -> BoxFuture<'_, Result<(), PersistenceError>> {
        let is_mysql = self.dialect() == "mysql";
        let esc = |val: &str| {
            let mut out = String::with_capacity(val.len() + 8);
            out.push('\'');
            for c in val.chars() {
                match c {
                    '\'' => out.push_str("''"),
                    '\\' if is_mysql => out.push_str("\\\\"),
                    '\0' if is_mysql => out.push_str("\\0"),
                    '\n' if is_mysql => out.push_str("\\n"),
                    '\r' if is_mysql => out.push_str("\\r"),
                    '\x1a' if is_mysql => out.push_str("\\Z"),
                    other => out.push(other),
                }
            }
            out.push('\'');
            out
        };
        let id_esc = esc(id);
        let action_esc = esc(action);
        let resource_esc = esc(resource);
        let status_esc = esc(status);
        let actor_esc = actor_id.map(esc).unwrap_or_else(|| "NULL".to_string());
        let target_esc = target_id.map(esc).unwrap_or_else(|| "NULL".to_string());
        let ip_esc = ip_address.map(esc).unwrap_or_else(|| "NULL".to_string());
        let meta_esc = esc(metadata);

        let stmt = format!(
            "INSERT INTO rume_audit_log (id, action, resource, status, actor_id, target_id, ip_address, metadata, created_at) VALUES ({id_esc}, {action_esc}, {resource_esc}, {status_esc}, {actor_esc}, {target_esc}, {ip_esc}, {meta_esc}, {created_at})"
        );
        Box::pin(async move {
            self.execute(&stmt).await?;
            Ok(())
        })
    }
}

#[cfg(any(feature = "postgres", feature = "mysql", feature = "sqlite"))]
pub(crate) fn map_sqlx_error(err: sqlx::Error) -> PersistenceError {
    match err {
        sqlx::Error::PoolTimedOut => PersistenceError::Timeout,
        sqlx::Error::PoolClosed => PersistenceError::Closed,
        sqlx::Error::Io(e) => PersistenceError::ConnectionFailed(e.to_string()),
        other => PersistenceError::QueryFailed(other.to_string()),
    }
}

/// Active transaction handle providing isolated mutation and explicit commit/rollback.
pub trait DatabaseTransaction: Debug + Send + Sync {
    /// Executes a statement within the transaction boundary.
    fn execute(&self, statement: &str) -> BoxFuture<'_, Result<u64, PersistenceError>>;

    /// Queries a scalar value within the transaction boundary.
    fn query_scalar(&self, query: &str) -> BoxFuture<'_, Result<Option<String>, PersistenceError>>;

    /// Atomically commits all changes made within this transaction.
    fn commit(&self) -> BoxFuture<'_, Result<(), PersistenceError>>;

    /// Explicitly discards and rolls back all changes made within this transaction.
    fn rollback(&self) -> BoxFuture<'_, Result<(), PersistenceError>>;
}
