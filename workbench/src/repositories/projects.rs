use rume::persistence::error::PersistenceError;
use rume::persistence::pool::DatabasePool;
use rume::persistence::postgres::PostgresBackend;
use sqlx::Row;

use crate::models::Project;

const SQL_CREATE: &str = include_str!("../../sql/postgres/projects/create.sql");
const SQL_LIST: &str = include_str!("../../sql/postgres/projects/list.sql");
const SQL_FIND: &str = include_str!("../../sql/postgres/projects/find.sql");

pub struct ProjectRepository;

impl ProjectRepository {
    pub async fn create(
        pool: &DatabasePool<PostgresBackend>,
        project: &Project,
    ) -> Result<(), PersistenceError> {
        sqlx::query(SQL_CREATE)
            .bind(&project.id)
            .bind(&project.tenant_id)
            .bind(&project.name)
            .bind(&project.description)
            .bind(project.created_at)
            .execute(pool.backend().pool())
            .await
            .map_err(|e| PersistenceError::QueryFailed(e.to_string()))?;
        Ok(())
    }

    pub async fn find(
        pool: &DatabasePool<PostgresBackend>,
        id: &str,
        tenant_id: &str,
    ) -> Result<Option<Project>, PersistenceError> {
        let row = sqlx::query(SQL_FIND)
            .bind(id)
            .bind(tenant_id)
            .fetch_optional(pool.backend().pool())
            .await
            .map_err(|e| PersistenceError::QueryFailed(e.to_string()))?;

        match row {
            Some(r) => Ok(Some(Self::map_row(&r)?)),
            None => Ok(None),
        }
    }

    pub async fn list_by_tenant(
        pool: &DatabasePool<PostgresBackend>,
        tenant_id: &str,
    ) -> Result<Vec<Project>, PersistenceError> {
        let rows = sqlx::query(SQL_LIST)
            .bind(tenant_id)
            .fetch_all(pool.backend().pool())
            .await
            .map_err(|e| PersistenceError::QueryFailed(e.to_string()))?;

        let mut projects = Vec::with_capacity(rows.len());
        for row in rows {
            projects.push(Self::map_row(&row)?);
        }
        Ok(projects)
    }

    fn map_row(row: &sqlx::postgres::PgRow) -> Result<Project, PersistenceError> {
        Ok(Project {
            id: row.try_get("id").map_err(|e| PersistenceError::QueryFailed(e.to_string()))?,
            tenant_id: row.try_get("tenant_id").map_err(|e| PersistenceError::QueryFailed(e.to_string()))?,
            name: row.try_get("name").map_err(|e| PersistenceError::QueryFailed(e.to_string()))?,
            description: row.try_get("description").map_err(|e| PersistenceError::QueryFailed(e.to_string()))?,
            created_at: row.try_get("created_at").map_err(|e| PersistenceError::QueryFailed(e.to_string()))?,
        })
    }
}
