//! Database backend and transaction abstractions.

use std::fmt::Debug;
use std::future::Future;
use std::pin::Pin;

use crate::persistence::error::PersistenceError;
use crate::persistence::row::DbRow;

/// Pinned boxed future type alias for async database traits.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A portable SQL parameter value supported across database backends.
#[derive(Debug, Clone, PartialEq)]
pub enum DbValue {
    /// SQL NULL value.
    Null,
    /// UTF-8 string or text value.
    Text(String),
    /// 64-bit signed integer.
    Int(i64),
    /// 64-bit floating point number.
    Float(f64),
    /// Boolean flag.
    Bool(bool),
    /// Raw byte sequence / BLOB.
    Bytes(Vec<u8>),
}

impl From<&str> for DbValue {
    fn from(s: &str) -> Self {
        DbValue::Text(s.to_string())
    }
}

impl From<String> for DbValue {
    fn from(s: String) -> Self {
        DbValue::Text(s)
    }
}

impl From<&String> for DbValue {
    fn from(s: &String) -> Self {
        DbValue::Text(s.clone())
    }
}

impl From<i8> for DbValue {
    fn from(i: i8) -> Self {
        DbValue::Int(i as i64)
    }
}

impl From<i16> for DbValue {
    fn from(i: i16) -> Self {
        DbValue::Int(i as i64)
    }
}

impl From<i32> for DbValue {
    fn from(i: i32) -> Self {
        DbValue::Int(i as i64)
    }
}

impl From<i64> for DbValue {
    fn from(i: i64) -> Self {
        DbValue::Int(i)
    }
}

impl From<isize> for DbValue {
    fn from(i: isize) -> Self {
        DbValue::Int(i as i64)
    }
}

impl From<u8> for DbValue {
    fn from(u: u8) -> Self {
        DbValue::Int(u as i64)
    }
}

impl From<u16> for DbValue {
    fn from(u: u16) -> Self {
        DbValue::Int(u as i64)
    }
}

impl From<u32> for DbValue {
    fn from(u: u32) -> Self {
        DbValue::Int(u as i64)
    }
}

impl From<u64> for DbValue {
    fn from(u: u64) -> Self {
        if let Ok(i) = i64::try_from(u) {
            DbValue::Int(i)
        } else {
            DbValue::Text(u.to_string())
        }
    }
}

impl From<usize> for DbValue {
    fn from(u: usize) -> Self {
        DbValue::Int(u as i64)
    }
}

impl From<f32> for DbValue {
    fn from(f: f32) -> Self {
        DbValue::Float(f as f64)
    }
}

impl From<f64> for DbValue {
    fn from(f: f64) -> Self {
        DbValue::Float(f)
    }
}

impl From<bool> for DbValue {
    fn from(b: bool) -> Self {
        DbValue::Bool(b)
    }
}

impl From<Vec<u8>> for DbValue {
    fn from(bytes: Vec<u8>) -> Self {
        DbValue::Bytes(bytes)
    }
}

impl From<&[u8]> for DbValue {
    fn from(bytes: &[u8]) -> Self {
        DbValue::Bytes(bytes.to_vec())
    }
}

impl<T: Into<DbValue>> From<Option<T>> for DbValue {
    fn from(opt: Option<T>) -> Self {
        match opt {
            Some(v) => v.into(),
            None => DbValue::Null,
        }
    }
}

impl<T: Clone + Into<DbValue>> From<&Option<T>> for DbValue {
    fn from(opt: &Option<T>) -> Self {
        match opt {
            Some(v) => v.clone().into(),
            None => DbValue::Null,
        }
    }
}

/// Convenience macro for constructing an array of [`DbValue`] parameters suitable for borrowing as a slice.
#[macro_export]
macro_rules! params {
    ($($val:expr),* $(,)?) => {
        [
            $(
                $crate::persistence::backend::DbValue::from($val)
            ),*
        ]
    };
}

/// Generic interface implemented by database drivers and storage backends.
pub trait DatabaseBackend: Debug + Send + Sync + 'static {
    /// Type of transaction produced by this backend.
    type Transaction: DatabaseTransaction + Clone + 'static;

    /// Verifies connectivity to the database.
    fn ping(&self) -> BoxFuture<'_, Result<(), PersistenceError>>;

    /// Executes a SQL statement directly with bound parameters, returning the number of affected rows.
    fn execute_params(
        &self,
        statement: &str,
        params: &[DbValue],
    ) -> BoxFuture<'_, Result<u64, PersistenceError>>;

    /// Executes a SQL or DDL statement directly, returning the number of affected rows.
    fn execute(&self, statement: &str) -> BoxFuture<'_, Result<u64, PersistenceError>> {
        self.execute_params(statement, &[])
    }

    /// Executes a scalar query with bound parameters returning an optional string value.
    fn query_scalar_params(
        &self,
        query: &str,
        params: &[DbValue],
    ) -> BoxFuture<'_, Result<Option<String>, PersistenceError>>;

    /// Executes a scalar query returning an optional string value.
    fn query_scalar(&self, query: &str) -> BoxFuture<'_, Result<Option<String>, PersistenceError>> {
        self.query_scalar_params(query, &[])
    }

    /// Fetches all rows matching the parameterized query as generic [`DbRow`] records.
    fn fetch_all(
        &self,
        statement: &str,
        params: &[DbValue],
    ) -> BoxFuture<'_, Result<Vec<DbRow>, PersistenceError>>;

    /// Fetches an optional single row matching the parameterized query as a [`DbRow`].
    fn fetch_optional(
        &self,
        statement: &str,
        params: &[DbValue],
    ) -> BoxFuture<'_, Result<Option<DbRow>, PersistenceError>>;

    /// Fetches exactly one row matching the parameterized query, returning an error if zero rows match.
    fn fetch_one(
        &self,
        statement: &str,
        params: &[DbValue],
    ) -> BoxFuture<'_, Result<DbRow, PersistenceError>> {
        let stmt = statement.to_string();
        let p = params.to_vec();
        Box::pin(async move {
            self.fetch_optional(&stmt, &p)
                .await?
                .ok_or_else(|| PersistenceError::QueryFailed("query returned zero rows, expected one".into()))
        })
    }

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
        let is_postgres = self.dialect() == "postgres";
        let stmt = if is_postgres {
            "INSERT INTO rume_audit_log (id, action, resource, status, actor_id, target_id, ip_address, metadata, created_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)"
        } else {
            "INSERT INTO rume_audit_log (id, action, resource, status, actor_id, target_id, ip_address, metadata, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)"
        };
        let params = vec![
            DbValue::from(id),
            DbValue::from(action),
            DbValue::from(resource),
            DbValue::from(status),
            DbValue::from(actor_id),
            DbValue::from(target_id),
            DbValue::from(ip_address),
            DbValue::from(metadata),
            DbValue::from(created_at),
        ];
        Box::pin(async move {
            self.execute_params(stmt, &params).await?;
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
    /// Returns the database dialect of this transaction (e.g. "postgres", "sqlite", "mysql", "default").
    fn dialect(&self) -> &'static str {
        "default"
    }

    /// Executes a parameterized statement within the transaction boundary.
    fn execute_params(
        &self,
        statement: &str,
        params: &[DbValue],
    ) -> BoxFuture<'_, Result<u64, PersistenceError>>;

    /// Executes a statement within the transaction boundary.
    fn execute(&self, statement: &str) -> BoxFuture<'_, Result<u64, PersistenceError>> {
        self.execute_params(statement, &[])
    }

    /// Queries a scalar value within the transaction boundary with bound parameters.
    fn query_scalar_params(
        &self,
        query: &str,
        params: &[DbValue],
    ) -> BoxFuture<'_, Result<Option<String>, PersistenceError>>;

    /// Queries a scalar value within the transaction boundary.
    fn query_scalar(&self, query: &str) -> BoxFuture<'_, Result<Option<String>, PersistenceError>> {
        self.query_scalar_params(query, &[])
    }

    /// Fetches all rows matching the parameterized query within the transaction boundary.
    fn fetch_all(
        &self,
        statement: &str,
        params: &[DbValue],
    ) -> BoxFuture<'_, Result<Vec<DbRow>, PersistenceError>>;

    /// Fetches an optional single row matching the parameterized query within the transaction boundary.
    fn fetch_optional(
        &self,
        statement: &str,
        params: &[DbValue],
    ) -> BoxFuture<'_, Result<Option<DbRow>, PersistenceError>>;

    /// Fetches exactly one row matching the parameterized query within the transaction boundary.
    fn fetch_one(
        &self,
        statement: &str,
        params: &[DbValue],
    ) -> BoxFuture<'_, Result<DbRow, PersistenceError>> {
        let stmt = statement.to_string();
        let p = params.to_vec();
        Box::pin(async move {
            self.fetch_optional(&stmt, &p)
                .await?
                .ok_or_else(|| PersistenceError::QueryFailed("query returned zero rows, expected one".into()))
        })
    }

    /// Atomically commits all changes made within this transaction.
    fn commit(&self) -> BoxFuture<'_, Result<(), PersistenceError>>;

    /// Explicitly discards and rolls back all changes made within this transaction.
    fn rollback(&self) -> BoxFuture<'_, Result<(), PersistenceError>>;
}
