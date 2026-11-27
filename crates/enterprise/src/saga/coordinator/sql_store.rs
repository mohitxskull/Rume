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
        self.save_record_cas_with_claim(record, expected_version, None)
    }

    fn save_record_cas_with_claim(
        &self,
        record: &SagaRecord,
        expected_version: u64,
        claim_token: Option<&str>,
    ) -> BoxFuture<'_, Result<(), SagaError>> {
        let record = record.clone();
        let claim_token = claim_token.map(str::to_string);
        Box::pin(async move {
            let (status_json, completed_json, compensated_json) = serialize_record(&record)?;
            let is_terminal = record.status.is_terminal();
            let updated_at = i64::try_from(record.updated_at).unwrap_or(i64::MAX);
            let expected_ver = i64::try_from(expected_version).unwrap_or(i64::MAX);

            let rows_affected = match self {
                #[cfg(feature = "sqlite")]
                Self::Sqlite { pool, claim_ttl } => {
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
                            .bind(claim_token.as_deref())
                            .bind(ttl_millis(*claim_ttl))
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
                            .bind(claim_token.as_deref())
                            .execute(pool)
                            .await
                            .map_err(|e| storage_err(Some(&record.saga_id), e))?
                            .rows_affected()
                    }
                }
                #[cfg(feature = "postgres")]
                Self::Postgres { pool, claim_ttl } => {
                    if expected_version == 0 {
                        sqlx::query(rume_data::sql::PG_SAGA_INSERT)
                            .bind(&record.saga_id)
                            .bind(&record.saga_name)
                            .bind(&status_json)
                            .bind(&completed_json)
                            .bind(&compensated_json)
                            .bind(is_terminal)
                            .bind(updated_at)
                            .bind(claim_token.as_deref())
                            .bind(ttl_millis(*claim_ttl))
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
                            .bind(claim_token.as_deref())
                            .execute(pool)
                            .await
                            .map_err(|e| storage_err(Some(&record.saga_id), e))?
                            .rows_affected()
                    }
                }
                #[cfg(feature = "mysql")]
                Self::Mysql { pool, claim_ttl } => {
                    let is_term_int = i8::from(is_terminal);
                    if expected_version == 0 {
                        // Why: MySQL positional `?` binds `claim_token` twice (once for the
                        // column value and once for `CASE WHEN ? IS NOT NULL`), then `ttl_ms`.
                        sqlx::query(rume_data::sql::MYSQL_SAGA_INSERT)
                            .bind(&record.saga_id)
                            .bind(&record.saga_name)
                            .bind(&status_json)
                            .bind(&completed_json)
                            .bind(&compensated_json)
                            .bind(is_term_int)
                            .bind(updated_at)
                            .bind(claim_token.as_deref())
                            .bind(claim_token.as_deref())
                            .bind(ttl_millis(*claim_ttl))
                            .execute(pool)
                            .await
                            .map_err(|e| storage_err(Some(&record.saga_id), e))?
                            .rows_affected()
                    } else {
                        // Why: MySQL uses positional `?` placeholders, so `SET` columns precede `WHERE`,
                        // `claim_token` is bound twice for `(? IS NULL OR (claim_token = ? AND claim_expires_at > ...))`,
                        // and lease freshness is evaluated against the DB server clock (`RC-01`).
                        sqlx::query(rume_data::sql::MYSQL_SAGA_UPDATE_CAS)
                            .bind(&record.saga_name)
                            .bind(&status_json)
                            .bind(&completed_json)
                            .bind(&compensated_json)
                            .bind(is_term_int)
                            .bind(updated_at)
                            .bind(&record.saga_id)
                            .bind(expected_ver)
                            .bind(claim_token.as_deref())
                            .bind(claim_token.as_deref())
                            .execute(pool)
                            .await
                            .map_err(|e| storage_err(Some(&record.saga_id), e))?
                            .rows_affected()
                    }
                }
            };

            if rows_affected == 0 {
                let message = if claim_token.is_some() && expected_version > 0 {
                    format!("saga version or claim conflict: expected version {expected_version}")
                } else {
                    format!("saga version conflict: expected version {expected_version}")
                };
                return Err(SagaError::Storage {
                    saga_id: Some(record.saga_id),
                    message,
                });
            }
            Ok(())
        })
    }

    fn create_and_claim_record<'a>(
        &'a self,
        record: &'a SagaRecord,
    ) -> BoxFuture<'a, Result<String, SagaError>> {
        let token = uuid::Uuid::new_v4().to_string();
        Box::pin(async move {
            self.save_record_cas_with_claim(record, 0, Some(&token))
                .await?;
            Ok(token)
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

    fn renew_execution(
        &self,
        saga_id: &str,
        claim_token: &str,
    ) -> BoxFuture<'_, Result<bool, SagaError>> {
        let saga_id = saga_id.to_string();
        let claim_token = claim_token.to_string();
        Box::pin(async move {
            let rows_affected = match self {
                #[cfg(feature = "sqlite")]
                Self::Sqlite { pool, claim_ttl } => {
                    sqlx::query(rume_data::sql::SQLITE_SAGA_RENEW_CLAIM)
                        .bind(&saga_id)
                        .bind(&claim_token)
                        .bind(ttl_millis(*claim_ttl))
                        .execute(pool)
                        .await
                        .map_err(|e| storage_err(Some(&saga_id), e))?
                        .rows_affected()
                }
                #[cfg(feature = "postgres")]
                Self::Postgres { pool, claim_ttl } => {
                    sqlx::query(rume_data::sql::PG_SAGA_RENEW_CLAIM)
                        .bind(&saga_id)
                        .bind(&claim_token)
                        .bind(ttl_millis(*claim_ttl))
                        .execute(pool)
                        .await
                        .map_err(|e| storage_err(Some(&saga_id), e))?
                        .rows_affected()
                }
                #[cfg(feature = "mysql")]
                Self::Mysql { pool, claim_ttl } => {
                    // Why: MySQL positional `?` binds `ttl_ms`, `saga_id`, then `claim_token`;
                    // lease expiry comparison uses the DB server clock (`RC-01`).
                    sqlx::query(rume_data::sql::MYSQL_SAGA_RENEW_CLAIM)
                        .bind(ttl_millis(*claim_ttl))
                        .bind(&saga_id)
                        .bind(&claim_token)
                        .execute(pool)
                        .await
                        .map_err(|e| storage_err(Some(&saga_id), e))?
                        .rows_affected()
                }
            };

            Ok(rows_affected > 0)
        })
    }

    fn claim_ttl(&self) -> Option<Duration> {
        match self {
            #[cfg(feature = "sqlite")]
            Self::Sqlite { claim_ttl, .. } => Some(*claim_ttl),
            #[cfg(feature = "postgres")]
            Self::Postgres { claim_ttl, .. } => Some(*claim_ttl),
            #[cfg(feature = "mysql")]
            Self::Mysql { claim_ttl, .. } => Some(*claim_ttl),
        }
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

    #[tokio::test]
    async fn resume_from_handle_succeeds_on_version_zero_handle_with_sql_store() {
        use crate::saga::coordinator::workflow::{Saga, SagaRecoveryHandle, SagaRecoveryPhase};
        use std::sync::Arc;

        let (_pool, store) = sqlite_store().await;
        let saga = Saga::<Vec<&'static str>>::new("v0_sql_recovery")
            .with_store(Arc::new(store))
            .step(
                "step_1",
                |ctx| {
                    ctx.push("step_1");
                    async { Ok(()) }
                },
                |_| async { Ok(()) },
            )
            .step(
                "step_2",
                |ctx| {
                    ctx.push("step_2");
                    async { Ok(()) }
                },
                |_| async { Ok(()) },
            );

        let handle = SagaRecoveryHandle {
            saga_id: "saga-v0-recovery".into(),
            saga_name: "v0_sql_recovery".into(),
            context: vec!["initial"],
            phase: SagaRecoveryPhase::Forward,
            completed_steps: Vec::new(),
            compensated_steps: Vec::new(),
            version: 0,
        };

        let report = saga
            .resume_from_handle(handle)
            .await
            .expect("resume_from_handle on version == 0 must succeed under SqlSagaStore");
        assert!(report.status.is_success());
        assert_eq!(report.context, vec!["initial", "step_1", "step_2"]);
        assert_eq!(
            report.completed_steps,
            vec!["step_1".to_string(), "step_2".to_string()]
        );
    }

    #[tokio::test]
    async fn sql_saga_store_rejects_stale_claim_token_cas() {
        let (_pool, store) = sqlite_store().await;
        let rec1 = SagaRecord {
            saga_id: "saga-fence".into(),
            saga_name: "order_flow".into(),
            status: SagaStatus::Pending,
            completed_steps: vec![],
            compensated_steps: vec![],
            updated_at: 300,
            version: 1,
        };
        store.save_record_cas(&rec1, 0).await.expect("insert");

        let token_a = store
            .try_acquire_execution("saga-fence")
            .await
            .expect("worker A acquires claim");
        assert!(
            store
                .renew_execution("saga-fence", &token_a)
                .await
                .expect("worker A renews active claim")
        );

        // Worker A releases and Worker B takes over the claim.
        store
            .release_execution("saga-fence", &token_a)
            .await
            .expect("worker A releases claim");
        let token_b = store
            .try_acquire_execution("saga-fence")
            .await
            .expect("worker B acquires claim");

        // Stale worker A attempts CAS with its old token -> MUST be rejected.
        let mut rec2 = rec1.clone();
        rec2.status = SagaStatus::Running {
            current_step: "step_1".into(),
        };
        rec2.version = 2;
        assert!(
            store
                .save_record_cas_with_claim(&rec2, 1, Some(&token_a))
                .await
                .is_err(),
            "stale claim_token CAS must be rejected when another worker holds the claim"
        );
        assert!(
            !store
                .renew_execution("saga-fence", &token_a)
                .await
                .expect("stale claim_token renewal should return Ok(false)"),
            "stale claim_token renewal must be rejected"
        );

        // Current claimant worker B succeeds with its token.
        store
            .save_record_cas_with_claim(&rec2, 1, Some(&token_b))
            .await
            .expect("active claimant CAS must succeed");

        let loaded = store
            .load_record("saga-fence")
            .await
            .expect("load")
            .expect("record exists");
        assert_eq!(loaded.version, 2);
    }

    #[tokio::test]
    async fn sql_saga_create_and_claim_fences_concurrent_claim_immediately() {
        let (_pool, store) = sqlite_store().await;
        let rec = SagaRecord {
            saga_id: "saga-atomic-claim".into(),
            saga_name: "order_flow".into(),
            status: SagaStatus::Pending,
            completed_steps: vec![],
            compensated_steps: vec![],
            updated_at: 400,
            version: 1,
        };
        let token = store
            .create_and_claim_record(&rec)
            .await
            .expect("atomic create_and_claim_record succeeds");

        // Concurrent worker attempting to acquire the claim immediately must fail!
        assert!(
            store
                .try_acquire_execution("saga-atomic-claim")
                .await
                .is_err(),
            "concurrent try_acquire_execution must fail while initial create+claim token is held"
        );

        // Release and verify claim becomes acquirable.
        store
            .release_execution("saga-atomic-claim", &token)
            .await
            .expect("release");
        assert!(
            store
                .try_acquire_execution("saga-atomic-claim")
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn resume_from_handle_reconciles_authoritative_record_after_contention() {
        use crate::saga::coordinator::workflow::{Saga, SagaRecoveryHandle, SagaRecoveryPhase};
        use std::sync::Arc;

        let (_pool, store) = sqlite_store().await;
        let store_arc = Arc::new(store.clone());

        // Persist a saga record at version 3 with step_1 completed and currently Compensating.
        let persisted = SagaRecord {
            saga_id: "saga-reconcile".into(),
            saga_name: "reconcile_flow".into(),
            status: SagaStatus::Pending,
            completed_steps: vec!["step_1".into()],
            compensated_steps: vec![],
            updated_at: 500,
            version: 1,
        };
        store.save_record_cas(&persisted, 0).await.unwrap();
        let mut persisted_v2 = persisted.clone();
        persisted_v2.status = SagaStatus::Compensating {
            current_step: "step_1".into(),
            error: Some(SagaError::ActionFailed {
                step: "step_2".into(),
                message: "boom".into(),
            }),
        };
        persisted_v2.version = 2;
        store.save_record_cas(&persisted_v2, 1).await.unwrap();

        // Hold claim while another caller tries to compensate_crashed_recoverable -> produces
        // a stale handle with version = 0, completed_steps = [].
        let saga = Saga::<Vec<&'static str>>::new("reconcile_flow")
            .with_store(store_arc)
            .step(
                "step_1",
                |ctx| {
                    ctx.push("exec_1");
                    async { Ok(()) }
                },
                |ctx| {
                    ctx.push("comp_1");
                    async { Ok(()) }
                },
            )
            .step(
                "step_2",
                |ctx| {
                    ctx.push("exec_2");
                    async { Ok(()) }
                },
                |ctx| {
                    ctx.push("comp_2");
                    async { Ok(()) }
                },
            );

        let stale_handle = SagaRecoveryHandle {
            saga_id: "saga-reconcile".into(),
            saga_name: "reconcile_flow".into(),
            context: vec!["ctx"],
            phase: SagaRecoveryPhase::Compensating {
                error: None,
                compensation_errors: vec![],
            },
            completed_steps: vec![],
            compensated_steps: vec![],
            version: 0,
        };

        // resume_from_handle MUST reconcile version=2 and completed_steps=["step_1"] from DB
        // instead of failing CAS with expected_version=0!
        let report = saga
            .resume_from_handle(stale_handle)
            .await
            .expect("resume_from_handle must reconcile authoritative SagaRecord");
        assert!(matches!(report.status, SagaStatus::Compensated { .. }));
        assert_eq!(report.context, vec!["ctx", "comp_1"]);
        assert_eq!(report.compensated_steps, vec!["step_1".to_string()]);
    }

    #[tokio::test]
    async fn resume_from_handle_reconciles_version_zero_forward_handle_against_persisted_saga() {
        use crate::saga::coordinator::workflow::{Saga, SagaRecoveryPhase};
        use std::sync::Arc;

        let (_pool, store) = sqlite_store().await;
        let store_arc = Arc::new(store.clone());

        // Persist a saga at version 2 with step_1 already completed and currently Running.
        let v1 = SagaRecord {
            saga_id: "saga-v0-forward".into(),
            saga_name: "forward_reconcile".into(),
            status: SagaStatus::Pending,
            completed_steps: vec![],
            compensated_steps: vec![],
            updated_at: 500,
            version: 1,
        };
        store.save_record_cas(&v1, 0).await.unwrap();
        let v2 = SagaRecord {
            status: SagaStatus::Running {
                current_step: "step_2".into(),
            },
            completed_steps: vec!["step_1".into()],
            version: 2,
            ..v1
        };
        store.save_record_cas(&v2, 1).await.unwrap();

        // Worker A holds the execution claim; Worker B calls `resume_recoverable`, which fails
        // claim acquisition and produces a stale `version == 0 && Forward` handle (`P2-05`).
        let worker_a_token = store
            .try_acquire_execution("saga-v0-forward")
            .await
            .expect("worker A acquires claim");

        let saga = Saga::<Vec<&'static str>>::new("forward_reconcile")
            .with_store(store_arc)
            .step(
                "step_1",
                |ctx| {
                    ctx.push("exec_1");
                    async { Ok(()) }
                },
                |_| async { Ok(()) },
            )
            .step(
                "step_2",
                |ctx| {
                    ctx.push("exec_2");
                    async { Ok(()) }
                },
                |_| async { Ok(()) },
            );

        let contended_err = match saga
            .resume_recoverable("saga-v0-forward", vec!["ctx"])
            .await
        {
            Err(err) => err,
            Ok(_) => panic!("worker B must fail while worker A holds claim"),
        };
        assert_eq!(contended_err.handle.version, 0);
        assert!(matches!(
            contended_err.handle.phase,
            SagaRecoveryPhase::Forward
        ));

        // Worker A releases its claim; retrying via `resume_from_handle` MUST reconcile
        // the persisted version=2 record and finish step_2 cleanly (no infinite CAS loop).
        store
            .release_execution("saga-v0-forward", &worker_a_token)
            .await
            .unwrap();

        let report = saga
            .resume_from_handle(*contended_err.handle)
            .await
            .expect("resume_from_handle must reconcile version == 0 Forward handle");
        assert!(matches!(report.status, SagaStatus::Success));
        assert_eq!(report.context, vec!["ctx", "exec_2"]);
        assert_eq!(
            report.completed_steps,
            vec!["step_1".to_string(), "step_2".to_string()]
        );
    }

    #[tokio::test]
    async fn sql_saga_store_rejects_renew_and_cas_after_claim_ttl_expiry() {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:")
            .await
            .expect("connect sqlite::memory:");
        let store = SqlSagaStore::sqlite(pool).with_claim_ttl(Duration::from_millis(20));
        store.init_schema().await.expect("init_schema");

        let rec1 = SagaRecord {
            saga_id: "saga-expire".into(),
            saga_name: "order_flow".into(),
            status: SagaStatus::Pending,
            completed_steps: vec![],
            compensated_steps: vec![],
            updated_at: 400,
            version: 1,
        };
        store.save_record_cas(&rec1, 0).await.expect("insert");

        let token = store
            .try_acquire_execution("saga-expire")
            .await
            .expect("acquire claim");

        // Wait past the 20ms claim TTL without any second worker taking over.
        tokio::time::sleep(Duration::from_millis(45)).await;

        let renew_res = store
            .renew_execution("saga-expire", &token)
            .await
            .expect("expired renewal should return Ok(false)");
        assert!(
            !renew_res,
            "expired worker must not be able to renew its own claim"
        );

        let mut rec2 = rec1.clone();
        rec2.status = SagaStatus::Running {
            current_step: "step_1".into(),
        };
        rec2.version = 2;
        assert!(
            store
                .save_record_cas_with_claim(&rec2, 1, Some(&token))
                .await
                .is_err(),
            "expired worker must not be able to commit a CAS update even when no second worker overwrote claim_token"
        );
    }
}
