//! SQL-backed durable [`SagaStore`] supporting SQLite, PostgreSQL, and MySQL.

#[cfg(any(feature = "sqlite", feature = "postgres", feature = "mysql"))]
use std::time::Duration;

#[cfg(any(feature = "sqlite", feature = "postgres", feature = "mysql"))]
use sqlx::Row;

#[cfg(any(feature = "sqlite", feature = "postgres", feature = "mysql"))]
use super::BoxFuture;
#[cfg(any(feature = "sqlite", feature = "postgres", feature = "mysql"))]
use super::store::{SagaRecord, SagaStatus, SagaStore};
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
    /// Creates a SQLite-backed saga store with a 60-second default claim lease TTL.
    #[cfg(feature = "sqlite")]
    pub fn sqlite(pool: sqlx::SqlitePool) -> Self {
        Self::Sqlite {
            pool,
            claim_ttl: Duration::from_secs(60),
        }
    }

    /// Creates a PostgreSQL-backed saga store with a 60-second default claim lease TTL.
    #[cfg(feature = "postgres")]
    pub fn postgres(pool: sqlx::PgPool) -> Self {
        Self::Postgres {
            pool,
            claim_ttl: Duration::from_secs(60),
        }
    }

    /// Creates a MySQL-backed saga store with a 60-second default claim lease TTL.
    #[cfg(feature = "mysql")]
    pub fn mysql(pool: sqlx::MySqlPool) -> Self {
        Self::Mysql {
            pool,
            claim_ttl: Duration::from_secs(60),
        }
    }

    /// Configures the execution claim lease TTL.
    pub fn with_claim_ttl(mut self, ttl: Duration) -> Self {
        match &mut self {
            #[cfg(feature = "sqlite")]
            Self::Sqlite { claim_ttl, .. } => *claim_ttl = ttl,
            #[cfg(feature = "postgres")]
            Self::Postgres { claim_ttl, .. } => *claim_ttl = ttl,
            #[cfg(feature = "mysql")]
            Self::Mysql { claim_ttl, .. } => *claim_ttl = ttl,
        }
        self
    }

    /// Initializes the `rume_sagas` table and indexes for the configured SQL dialect.
    pub async fn init_schema(&self) -> Result<(), SagaError> {
        match self {
            #[cfg(feature = "sqlite")]
            Self::Sqlite { pool, .. } => {
                sqlx::raw_sql(rume_data::sql::SQLITE_SAGA_SCHEMA)
                    .execute(pool)
                    .await
                    .map_err(|e| storage_err(None, e))?;
            }
            #[cfg(feature = "postgres")]
            Self::Postgres { pool, .. } => {
                sqlx::raw_sql(rume_data::sql::PG_SAGA_SCHEMA)
                    .execute(pool)
                    .await
                    .map_err(|e| storage_err(None, e))?;
            }
            #[cfg(feature = "mysql")]
            Self::Mysql { pool, .. } => {
                sqlx::raw_sql(rume_data::sql::MYSQL_SAGA_SCHEMA)
                    .execute(pool)
                    .await
                    .map_err(|e| storage_err(None, e))?;
            }
        }
        Ok(())
    }
}

#[cfg(any(feature = "sqlite", feature = "postgres", feature = "mysql"))]
fn storage_err(saga_id: Option<&str>, err: impl std::fmt::Display) -> SagaError {
    SagaError::Storage {
        saga_id: saga_id.map(str::to_string),
        message: err.to_string(),
    }
}

#[cfg(any(feature = "sqlite", feature = "postgres", feature = "mysql"))]
fn serialize_record(record: &SagaRecord) -> Result<(String, String, String), SagaError> {
    let id = Some(record.saga_id.as_str());
    let status_json = serde_json::to_string(&record.status).map_err(|e| storage_err(id, e))?;
    let completed_json =
        serde_json::to_string(&record.completed_steps).map_err(|e| storage_err(id, e))?;
    let compensated_json =
        serde_json::to_string(&record.compensated_steps).map_err(|e| storage_err(id, e))?;
    Ok((status_json, completed_json, compensated_json))
}

#[cfg(any(feature = "sqlite", feature = "postgres", feature = "mysql"))]
fn decode_record(
    saga_id: String,
    saga_name: String,
    status_json: &str,
    completed_json: &str,
    compensated_json: &str,
    updated_at: i64,
    version: i64,
) -> Result<SagaRecord, SagaError> {
    let status: SagaStatus =
        serde_json::from_str(status_json).map_err(|e| storage_err(Some(&saga_id), e))?;
    let completed_steps: Vec<String> =
        serde_json::from_str(completed_json).map_err(|e| storage_err(Some(&saga_id), e))?;
    let compensated_steps: Vec<String> =
        serde_json::from_str(compensated_json).map_err(|e| storage_err(Some(&saga_id), e))?;
    Ok(SagaRecord {
        saga_id,
        saga_name,
        status,
        completed_steps,
        compensated_steps,
        updated_at: updated_at.max(0) as u64,
        version: version.max(0) as u64,
    })
}

#[cfg(any(feature = "sqlite", feature = "postgres", feature = "mysql"))]
fn parse_row<'r, R>(row: &'r R) -> Result<SagaRecord, SagaError>
where
    R: Row,
    &'r str: sqlx::ColumnIndex<R>,
    String: sqlx::Decode<'r, R::Database> + sqlx::Type<R::Database>,
    i64: sqlx::Decode<'r, R::Database> + sqlx::Type<R::Database>,
{
    let saga_id: String = row.try_get("saga_id").map_err(|e| storage_err(None, e))?;
    let saga_name: String = row
        .try_get("saga_name")
        .map_err(|e| storage_err(Some(&saga_id), e))?;
    let status_json: String = row
        .try_get("status_json")
        .map_err(|e| storage_err(Some(&saga_id), e))?;
    let completed_json: String = row
        .try_get("completed_steps_json")
        .map_err(|e| storage_err(Some(&saga_id), e))?;
    let compensated_json: String = row
        .try_get("compensated_steps_json")
        .map_err(|e| storage_err(Some(&saga_id), e))?;
    let updated_at: i64 = row
        .try_get("updated_at")
        .map_err(|e| storage_err(Some(&saga_id), e))?;
    let version: i64 = row
        .try_get("version")
        .map_err(|e| storage_err(Some(&saga_id), e))?;
    decode_record(
        saga_id,
        saga_name,
        &status_json,
        &completed_json,
        &compensated_json,
        updated_at,
        version,
    )
}

#[cfg(any(feature = "sqlite", feature = "postgres", feature = "mysql"))]
fn ttl_millis(ttl: Duration) -> i64 {
    i64::try_from(ttl.as_millis().max(1)).unwrap_or(i64::MAX)
}

#[cfg(any(feature = "sqlite", feature = "postgres", feature = "mysql"))]
impl SagaStore for SqlSagaStore {
    fn save_record_cas(
        &self,
        record: &SagaRecord,
        expected_version: u64,
    ) -> BoxFuture<'_, Result<(), SagaError>> {
        let record = record.clone();
        Box::pin(async move {
            let (status_json, completed_json, compensated_json) = serialize_record(&record)?;
            let is_terminal = record.status.is_terminal();
            let updated_at = i64::try_from(record.updated_at).unwrap_or(i64::MAX);
            let expected_ver = i64::try_from(expected_version).unwrap_or(i64::MAX);

            let rows_affected = match self {
                #[cfg(feature = "sqlite")]
                Self::Sqlite { pool, .. } => {
                    let is_term_int = i32::from(is_terminal);
                    if expected_version == 0 {
                        sqlx::query(rume_data::sql::SQLITE_SAGA_INSERT)
                            .bind(&record.saga_id)
                            .bind(&record.saga_name)
                            .bind(&status_json)
                            .bind(&completed_json)
                            .bind(&compensated_json)
                            .bind(is_term_int)
                            .bind(updated_at)
                            .execute(pool)
                            .await
                            .map_err(|e| storage_err(Some(&record.saga_id), e))?
                            .rows_affected()
                    } else {
                        sqlx::query(rume_data::sql::SQLITE_SAGA_UPDATE_CAS)
                            .bind(&record.saga_id)
                            .bind(&record.saga_name)
                            .bind(&status_json)
                            .bind(&completed_json)
                            .bind(&compensated_json)
                            .bind(is_term_int)
                            .bind(updated_at)
                            .bind(expected_ver)
                            .execute(pool)
                            .await
                            .map_err(|e| storage_err(Some(&record.saga_id), e))?
                            .rows_affected()
                    }
                }
                #[cfg(feature = "postgres")]
                Self::Postgres { pool, .. } => {
                    if expected_version == 0 {
                        sqlx::query(rume_data::sql::PG_SAGA_INSERT)
                            .bind(&record.saga_id)
                            .bind(&record.saga_name)
                            .bind(&status_json)
                            .bind(&completed_json)
                            .bind(&compensated_json)
                            .bind(is_terminal)
                            .bind(updated_at)
                            .execute(pool)
                            .await
                            .map_err(|e| storage_err(Some(&record.saga_id), e))?
                            .rows_affected()
                    } else {
                        sqlx::query(rume_data::sql::PG_SAGA_UPDATE_CAS)
                            .bind(&record.saga_id)
                            .bind(&record.saga_name)
                            .bind(&status_json)
                            .bind(&completed_json)
                            .bind(&compensated_json)
                            .bind(is_terminal)
                            .bind(updated_at)
                            .bind(expected_ver)
                            .execute(pool)
                            .await
                            .map_err(|e| storage_err(Some(&record.saga_id), e))?
                            .rows_affected()
                    }
                }
                #[cfg(feature = "mysql")]
                Self::Mysql { pool, .. } => {
                    let is_term_int = i8::from(is_terminal);
                    if expected_version == 0 {
                        sqlx::query(rume_data::sql::MYSQL_SAGA_INSERT)
                            .bind(&record.saga_id)
                            .bind(&record.saga_name)
                            .bind(&status_json)
                            .bind(&completed_json)
                            .bind(&compensated_json)
                            .bind(is_term_int)
                            .bind(updated_at)
                            .execute(pool)
                            .await
                            .map_err(|e| storage_err(Some(&record.saga_id), e))?
                            .rows_affected()
                    } else {
                        // Why: MySQL uses positional `?` placeholders, so `SET` columns precede `WHERE`.
                        sqlx::query(rume_data::sql::MYSQL_SAGA_UPDATE_CAS)
                            .bind(&record.saga_name)
                            .bind(&status_json)
                            .bind(&completed_json)
                            .bind(&compensated_json)
                            .bind(is_term_int)
                            .bind(updated_at)
                            .bind(&record.saga_id)
                            .bind(expected_ver)
                            .execute(pool)
                            .await
                            .map_err(|e| storage_err(Some(&record.saga_id), e))?
                            .rows_affected()
                    }
                }
            };

            if rows_affected == 0 {
                return Err(SagaError::Storage {
                    saga_id: Some(record.saga_id),
                    message: format!("saga version conflict: expected version {expected_version}"),
                });
            }
            Ok(())
        })
    }

    fn load_record(&self, saga_id: &str) -> BoxFuture<'_, Result<Option<SagaRecord>, SagaError>> {
        let saga_id = saga_id.to_string();
        Box::pin(async move {
            match self {
                #[cfg(feature = "sqlite")]
                Self::Sqlite { pool, .. } => {
                    let row = sqlx::query(rume_data::sql::SQLITE_SAGA_LOAD)
                        .bind(&saga_id)
                        .fetch_optional(pool)
                        .await
                        .map_err(|e| storage_err(Some(&saga_id), e))?;
                    row.as_ref().map(parse_row).transpose()
                }
                #[cfg(feature = "postgres")]
                Self::Postgres { pool, .. } => {
                    let row = sqlx::query(rume_data::sql::PG_SAGA_LOAD)
                        .bind(&saga_id)
                        .fetch_optional(pool)
                        .await
                        .map_err(|e| storage_err(Some(&saga_id), e))?;
                    row.as_ref().map(parse_row).transpose()
                }
                #[cfg(feature = "mysql")]
                Self::Mysql { pool, .. } => {
                    let row = sqlx::query(rume_data::sql::MYSQL_SAGA_LOAD)
                        .bind(&saga_id)
                        .fetch_optional(pool)
                        .await
                        .map_err(|e| storage_err(Some(&saga_id), e))?;
                    row.as_ref().map(parse_row).transpose()
                }
            }
        })
    }

    fn list_incomplete(&self) -> BoxFuture<'_, Result<Vec<SagaRecord>, SagaError>> {
        Box::pin(async move {
            match self {
                #[cfg(feature = "sqlite")]
                Self::Sqlite { pool, .. } => {
                    let rows = sqlx::query(rume_data::sql::SQLITE_SAGA_LIST_INCOMPLETE)
                        .fetch_all(pool)
                        .await
                        .map_err(|e| storage_err(None, e))?;
                    rows.iter().map(parse_row).collect()
                }
                #[cfg(feature = "postgres")]
                Self::Postgres { pool, .. } => {
                    let rows = sqlx::query(rume_data::sql::PG_SAGA_LIST_INCOMPLETE)
                        .fetch_all(pool)
                        .await
                        .map_err(|e| storage_err(None, e))?;
                    rows.iter().map(parse_row).collect()
                }
                #[cfg(feature = "mysql")]
                Self::Mysql { pool, .. } => {
                    let rows = sqlx::query(rume_data::sql::MYSQL_SAGA_LIST_INCOMPLETE)
                        .fetch_all(pool)
                        .await
                        .map_err(|e| storage_err(None, e))?;
                    rows.iter().map(parse_row).collect()
                }
            }
        })
    }

    fn try_acquire_execution(&self, saga_id: &str) -> BoxFuture<'_, Result<String, SagaError>> {
        let saga_id = saga_id.to_string();
        Box::pin(async move {
            let token = uuid::Uuid::new_v4().to_string();
            let rows_affected = match self {
                #[cfg(feature = "sqlite")]
                Self::Sqlite { pool, claim_ttl } => {
                    sqlx::query(rume_data::sql::SQLITE_SAGA_ACQUIRE_CLAIM)
                        .bind(&saga_id)
                        .bind(&token)
                        .bind(ttl_millis(*claim_ttl))
                        .execute(pool)
                        .await
                        .map_err(|e| storage_err(Some(&saga_id), e))?
                        .rows_affected()
                }
                #[cfg(feature = "postgres")]
                Self::Postgres { pool, claim_ttl } => {
                    sqlx::query(rume_data::sql::PG_SAGA_ACQUIRE_CLAIM)
                        .bind(&saga_id)
                        .bind(&token)
                        .bind(ttl_millis(*claim_ttl))
                        .execute(pool)
                        .await
                        .map_err(|e| storage_err(Some(&saga_id), e))?
                        .rows_affected()
                }
                #[cfg(feature = "mysql")]
                Self::Mysql { pool, claim_ttl } => {
                    // Why: MySQL positional `?` binds `claim_token`, `ttl_ms`, then `saga_id`.
                    sqlx::query(rume_data::sql::MYSQL_SAGA_ACQUIRE_CLAIM)
                        .bind(&token)
                        .bind(ttl_millis(*claim_ttl))
                        .bind(&saga_id)
                        .execute(pool)
                        .await
                        .map_err(|e| storage_err(Some(&saga_id), e))?
                        .rows_affected()
                }
            };

            if rows_affected == 0 {
                return Err(SagaError::Storage {
                    saga_id: Some(saga_id),
                    message: "saga execution already claimed by another worker".into(),
                });
            }
            Ok(token)
        })
    }

    fn release_execution(
        &self,
        saga_id: &str,
        claim_token: &str,
    ) -> BoxFuture<'_, Result<(), SagaError>> {
        let saga_id = saga_id.to_string();
        let claim_token = claim_token.to_string();
        Box::pin(async move {
            match self {
                #[cfg(feature = "sqlite")]
                Self::Sqlite { pool, .. } => {
                    sqlx::query(rume_data::sql::SQLITE_SAGA_RELEASE_CLAIM)
                        .bind(&saga_id)
                        .bind(&claim_token)
                        .execute(pool)
                        .await
                        .map_err(|e| storage_err(Some(&saga_id), e))?;
                }
                #[cfg(feature = "postgres")]
                Self::Postgres { pool, .. } => {
                    sqlx::query(rume_data::sql::PG_SAGA_RELEASE_CLAIM)
                        .bind(&saga_id)
                        .bind(&claim_token)
                        .execute(pool)
                        .await
                        .map_err(|e| storage_err(Some(&saga_id), e))?;
                }
                #[cfg(feature = "mysql")]
                Self::Mysql { pool, .. } => {
                    sqlx::query(rume_data::sql::MYSQL_SAGA_RELEASE_CLAIM)
                        .bind(&saga_id)
                        .bind(&claim_token)
                        .execute(pool)
                        .await
                        .map_err(|e| storage_err(Some(&saga_id), e))?;
                }
            }
            Ok(())
        })
    }
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use super::*;
    use crate::saga::coordinator::store::SagaStatus;

    async fn sqlite_store() -> (sqlx::SqlitePool, SqlSagaStore) {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:")
            .await
            .expect("connect sqlite::memory:");
        let store = SqlSagaStore::sqlite(pool.clone());
        store.init_schema().await.expect("init_schema");
        (pool, store)
    }

    #[tokio::test]
    async fn sql_saga_store_enforces_cas_and_recovers_incomplete_sagas() {
        let (pool, store) = sqlite_store().await;
        let rec1 = SagaRecord {
            saga_id: "saga-1".into(),
            saga_name: "order_flow".into(),
            status: SagaStatus::Running {
                current_step: "reserve".into(),
            },
            completed_steps: vec![],
            compensated_steps: vec![],
            updated_at: 100,
            version: 1,
        };
        store
            .save_record_cas(&rec1, 0)
            .await
            .expect("initial insert with expected_version=0");

        // Duplicate insert with expected_version=0 must fail CAS.
        assert!(store.save_record_cas(&rec1, 0).await.is_err());

        // Stale version update must fail CAS.
        let mut rec2 = rec1.clone();
        rec2.completed_steps.push("reserve".into());
        rec2.version = 2;
        assert!(store.save_record_cas(&rec2, 99).await.is_err());

        // Correct expected_version=1 succeeds.
        store
            .save_record_cas(&rec2, 1)
            .await
            .expect("cas update with expected_version=1");

        // Persistence across store instances.
        let store2 = SqlSagaStore::sqlite(pool);
        let loaded = store2
            .load_record("saga-1")
            .await
            .expect("load_record")
            .expect("record should exist");
        assert_eq!(loaded.version, 2);
        assert_eq!(loaded.completed_steps, vec!["reserve".to_string()]);

        // Insert a terminal saga and verify list_incomplete returns only saga-1.
        let done = SagaRecord {
            saga_id: "saga-done".into(),
            saga_name: "order_flow".into(),
            status: SagaStatus::Success,
            completed_steps: vec!["reserve".into()],
            compensated_steps: vec![],
            updated_at: 101,
            version: 1,
        };
        store2.save_record_cas(&done, 0).await.expect("insert done");
        let incomplete = store2.list_incomplete().await.expect("list_incomplete");
        assert_eq!(incomplete.len(), 1);
        assert_eq!(incomplete[0].saga_id, "saga-1");
    }

    #[tokio::test]
    async fn sql_saga_store_fences_concurrent_execution_claims() {
        let (_pool, store) = sqlite_store().await;
        let rec = SagaRecord {
            saga_id: "saga-claim".into(),
            saga_name: "order_flow".into(),
            status: SagaStatus::Pending,
            completed_steps: vec![],
            compensated_steps: vec![],
            updated_at: 200,
            version: 1,
        };
        store.save_record_cas(&rec, 0).await.expect("insert");

        let token = store
            .try_acquire_execution("saga-claim")
            .await
            .expect("first claim succeeds");
        assert!(
            store.try_acquire_execution("saga-claim").await.is_err(),
            "second claim must fail while first claim is held"
        );

        // Wrong token must not release the claim.
        store
            .release_execution("saga-claim", "wrong-token")
            .await
            .expect("wrong-token release is no-op");
        assert!(store.try_acquire_execution("saga-claim").await.is_err());

        // Matching token releases the claim, allowing re-acquisition.
        store
            .release_execution("saga-claim", &token)
            .await
            .expect("release with matching token");
        let token2 = store
            .try_acquire_execution("saga-claim")
            .await
            .expect("re-acquire after release");
        assert_ne!(token, token2);
    }
}
