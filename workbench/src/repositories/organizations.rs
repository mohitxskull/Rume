use rume::persistence::error::PersistenceError;
use rume::persistence::pool::DatabasePool;
use rume::persistence::postgres::PostgresBackend;
use sqlx::Row;

use crate::models::Organization;

const SQL_CREATE: &str = include_str!("../../sql/postgres/organizations/create.sql");
const SQL_LIST: &str = include_str!("../../sql/postgres/organizations/list.sql");
const SQL_FIND: &str = include_str!("../../sql/postgres/organizations/find.sql");

pub struct OrganizationRepository;

impl OrganizationRepository {
    pub async fn create(
        pool: &DatabasePool<PostgresBackend>,
        org: &Organization,
    ) -> Result<(), PersistenceError> {
        sqlx::query(SQL_CREATE)
            .bind(&org.id)
            .bind(&org.name)
            .bind(org.created_at)
            .execute(pool.backend().pool())
            .await
            .map_err(|e| PersistenceError::QueryFailed(e.to_string()))?;
        Ok(())
    }

    pub async fn find(
        pool: &DatabasePool<PostgresBackend>,
        id: &str,
    ) -> Result<Option<Organization>, PersistenceError> {
        let row = sqlx::query(SQL_FIND)
            .bind(id)
            .fetch_optional(pool.backend().pool())
            .await
            .map_err(|e| PersistenceError::QueryFailed(e.to_string()))?;

        match row {
            Some(r) => Ok(Some(Self::map_row(&r)?)),
            None => Ok(None),
        }
    }

    pub async fn list_all(
        pool: &DatabasePool<PostgresBackend>,
    ) -> Result<Vec<Organization>, PersistenceError> {
        let rows = sqlx::query(SQL_LIST)
            .fetch_all(pool.backend().pool())
            .await
            .map_err(|e| PersistenceError::QueryFailed(e.to_string()))?;

        let mut orgs = Vec::with_capacity(rows.len());
        for row in rows {
            orgs.push(Self::map_row(&row)?);
        }
        Ok(orgs)
    }

    fn map_row(row: &sqlx::postgres::PgRow) -> Result<Organization, PersistenceError> {
        Ok(Organization {
            id: row.try_get("id").map_err(|e| PersistenceError::QueryFailed(e.to_string()))?,
            name: row.try_get("name").map_err(|e| PersistenceError::QueryFailed(e.to_string()))?,
            created_at: row.try_get("created_at").map_err(|e| PersistenceError::QueryFailed(e.to_string()))?,
        })
    }
}
