//! First-class SQL query asset definition and execution interface.

use std::fmt::Display;

use crate::persistence::backend::{DatabaseBackend, DatabaseTransaction, DbValue};
use crate::persistence::error::PersistenceError;
use crate::persistence::pool::DatabasePool;
use crate::persistence::row::{DbRow, FromRow};

/// A statically declared, first-class SQL query asset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SqlQuery<'a> {
    /// Logical name of the query (e.g. "issues.create" or "projects.list").
    pub name: &'a str,
    /// Originating asset path (e.g. "sql/postgres/issues/create.sql").
    pub path: &'a str,
    /// Target database dialect if dialect-specific (e.g. "postgres", "mysql", "sqlite").
    pub dialect: Option<&'a str>,
    /// The embedded SQL statement string.
    pub sql: &'a str,
}

impl<'a> SqlQuery<'a> {
    /// Creates a new SQL query asset reference.
    pub const fn new(name: &'a str, path: &'a str, dialect: Option<&'a str>, sql: &'a str) -> Self {
        Self {
            name,
            path,
            dialect,
            sql,
        }
    }

    /// Returns the raw SQL statement string.
    pub fn as_str(&self) -> &'a str {
        self.sql
    }

    /// Executes the query statement within a transaction with bound parameters, returning affected rows.
    pub async fn execute<T: DatabaseTransaction>(
        &self,
        tx: &T,
        params: &[DbValue],
    ) -> Result<u64, PersistenceError> {
        tx.execute_params(self.sql, params).await
    }

    /// Executes the query statement directly on a database pool with bound parameters, returning affected rows.
    pub async fn execute_pool<B: DatabaseBackend>(
        &self,
        pool: &DatabasePool<B>,
        params: &[DbValue],
    ) -> Result<u64, PersistenceError> {
        pool.execute_params(self.sql, params).await
    }

    /// Executes the query on a database pool and fetches all matching rows as generic [`DbRow`] records.
    pub async fn fetch_all<B: DatabaseBackend>(
        &self,
        pool: &DatabasePool<B>,
        params: &[DbValue],
    ) -> Result<Vec<DbRow>, PersistenceError> {
        pool.fetch_all(self.sql, params).await
    }

    /// Executes the query on a database pool and maps all matching rows into type `R` via [`FromRow`].
    pub async fn fetch_all_as<B: DatabaseBackend, R: FromRow>(
        &self,
        pool: &DatabasePool<B>,
        params: &[DbValue],
    ) -> Result<Vec<R>, PersistenceError> {
        pool.fetch_all_as(self.sql, params).await
    }

    /// Executes the query within a transaction and fetches all matching rows as generic [`DbRow`] records.
    pub async fn fetch_all_tx<T: DatabaseTransaction>(
        &self,
        tx: &T,
        params: &[DbValue],
    ) -> Result<Vec<DbRow>, PersistenceError> {
        tx.fetch_all(self.sql, params).await
    }

    /// Executes the query within a transaction and maps all matching rows into type `R` via [`FromRow`].
    pub async fn fetch_all_tx_as<T: DatabaseTransaction, R: FromRow>(
        &self,
        tx: &T,
        params: &[DbValue],
    ) -> Result<Vec<R>, PersistenceError> {
        let rows = tx.fetch_all(self.sql, params).await?;
        rows.iter().map(R::from_row).collect()
    }

    /// Executes the query on a database pool and fetches an optional row as [`DbRow`].
    pub async fn fetch_optional<B: DatabaseBackend>(
        &self,
        pool: &DatabasePool<B>,
        params: &[DbValue],
    ) -> Result<Option<DbRow>, PersistenceError> {
        pool.fetch_optional(self.sql, params).await
    }

    /// Executes the query on a database pool and maps an optional row into type `R` via [`FromRow`].
    pub async fn fetch_optional_as<B: DatabaseBackend, R: FromRow>(
        &self,
        pool: &DatabasePool<B>,
        params: &[DbValue],
    ) -> Result<Option<R>, PersistenceError> {
        pool.fetch_optional_as(self.sql, params).await
    }

    /// Executes the query within a transaction and fetches an optional row as [`DbRow`].
    pub async fn fetch_optional_tx<T: DatabaseTransaction>(
        &self,
        tx: &T,
        params: &[DbValue],
    ) -> Result<Option<DbRow>, PersistenceError> {
        tx.fetch_optional(self.sql, params).await
    }

    /// Executes the query within a transaction and maps an optional row into type `R` via [`FromRow`].
    pub async fn fetch_optional_tx_as<T: DatabaseTransaction, R: FromRow>(
        &self,
        tx: &T,
        params: &[DbValue],
    ) -> Result<Option<R>, PersistenceError> {
        match tx.fetch_optional(self.sql, params).await? {
            Some(row) => R::from_row(&row).map(Some),
            None => Ok(None),
        }
    }

    /// Executes the query on a database pool and fetches exactly one row as [`DbRow`].
    pub async fn fetch_one<B: DatabaseBackend>(
        &self,
        pool: &DatabasePool<B>,
        params: &[DbValue],
    ) -> Result<DbRow, PersistenceError> {
        pool.fetch_one(self.sql, params).await
    }

    /// Executes the query on a database pool and maps exactly one row into type `R` via [`FromRow`].
    pub async fn fetch_one_as<B: DatabaseBackend, R: FromRow>(
        &self,
        pool: &DatabasePool<B>,
        params: &[DbValue],
    ) -> Result<R, PersistenceError> {
        pool.fetch_one_as(self.sql, params).await
    }

    /// Executes the query and returns an optional scalar string from the first column of the first row.
    pub async fn query_scalar<B: DatabaseBackend>(
        &self,
        pool: &DatabasePool<B>,
        params: &[DbValue],
    ) -> Result<Option<String>, PersistenceError> {
        pool.query_scalar_params(self.sql, params).await
    }

    /// Executes the query within a transaction and returns an optional scalar string.
    pub async fn query_scalar_tx<T: DatabaseTransaction>(
        &self,
        tx: &T,
        params: &[DbValue],
    ) -> Result<Option<String>, PersistenceError> {
        tx.query_scalar_params(self.sql, params).await
    }
}

impl<'a> Display for SqlQuery<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.sql)
    }
}

impl<'a> AsRef<str> for SqlQuery<'a> {
    fn as_ref(&self) -> &str {
        self.sql
    }
}
