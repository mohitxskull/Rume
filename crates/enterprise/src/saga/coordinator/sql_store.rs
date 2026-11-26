//! SQL-backed durable [`SagaStore`] supporting SQLite, PostgreSQL, and MySQL.

#[cfg(any(feature = "sqlite", feature = "postgres", feature = "mysql"))]
use std::time::Duration;

#[cfg(any(feature = "sqlite", feature = "postgres", feature = "mysql"))]
use super::BoxFuture;
#[cfg(any(feature = "sqlite", feature = "postgres", feature = "mysql"))]
use super::store::{SagaRecord, SagaStore};
#[cfg(any(feature = "sqlite", feature = "postgres", feature = "mysql"))]
use crate::saga::error::SagaError;

/// Durable SQL-backed [`SagaStore`] with atomic CAS and lease-fenced execution claims.
#[cfg(any(feature = "sqlite", feature = "postgres", feature = "mysql"))]
#[derive(Debug, Clone)]
pub enum SqlSagaStore {
    /// SQLite-backed saga store.
    #[cfg(feature = "sqlite")]
    Sqlite {
        pool: sqlx::SqlitePool,
        claim_ttl: Duration,
    },
    /// PostgreSQL-backed saga store.
    #[cfg(feature = "postgres")]
    Postgres {
        pool: sqlx::PgPool,
        claim_ttl: Duration,
    },
    /// MySQL-backed saga store.
    #[cfg(feature = "mysql")]
    Mysql {
        pool: sqlx::MySqlPool,
        claim_ttl: Duration,
    },
}

#[cfg(any(feature = "sqlite", feature = "postgres", feature = "mysql"))]
impl SqlSagaStore {
    /// Initializes the `rume_sagas` table and indexes for the configured SQL dialect.
    pub async fn init_schema(&self) -> Result<(), SagaError> {
        todo!("Task 06: implement SqlSagaStore::init_schema")
    }
}

#[cfg(any(feature = "sqlite", feature = "postgres", feature = "mysql"))]
impl SagaStore for SqlSagaStore {
    fn save_record_cas(
        &self,
        _record: &SagaRecord,
        _expected_version: u64,
    ) -> BoxFuture<'_, Result<(), SagaError>> {
        todo!("Task 06: implement SqlSagaStore::save_record_cas")
    }

    fn load_record(&self, _saga_id: &str) -> BoxFuture<'_, Result<Option<SagaRecord>, SagaError>> {
        todo!("Task 06: implement SqlSagaStore::load_record")
    }

    fn list_incomplete(&self) -> BoxFuture<'_, Result<Vec<SagaRecord>, SagaError>> {
        todo!("Task 06: implement SqlSagaStore::list_incomplete")
    }

    fn try_acquire_execution(&self, _saga_id: &str) -> BoxFuture<'_, Result<String, SagaError>> {
        todo!("Task 06: implement SqlSagaStore::try_acquire_execution")
    }

    fn release_execution(
        &self,
        _saga_id: &str,
        _claim_token: &str,
    ) -> BoxFuture<'_, Result<(), SagaError>> {
        todo!("Task 06: implement SqlSagaStore::release_execution")
    }
}
