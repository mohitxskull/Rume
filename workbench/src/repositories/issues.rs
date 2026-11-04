use rume::persistence::backend::DatabaseTransaction;
use rume::persistence::error::PersistenceError;
use rume::persistence::pool::DatabasePool;
use rume::persistence::postgres::PostgresBackend;
use sqlx::Row;

use crate::models::Issue;

const SQL_CREATE: &str = include_str!("../../sql/postgres/issues/create.sql");
const SQL_UPDATE: &str = include_str!("../../sql/postgres/issues/update.sql");
const SQL_LIST: &str = include_str!("../../sql/postgres/issues/list.sql");
const SQL_FIND: &str = include_str!("../../sql/postgres/issues/find.sql");

pub struct IssueRepository;

impl IssueRepository {
    pub async fn create_in_tx<T: DatabaseTransaction>(tx: &T, issue: &Issue) -> Result<(), PersistenceError> {
        tx.execute_params(
            SQL_CREATE,
            &rume::params![
                &issue.id,
                &issue.tenant_id,
                &issue.project_id,
                &issue.title,
                &issue.description,
                &issue.status,
                &issue.priority,
                &issue.assignee_id,
                issue.created_at,
                issue.updated_at,
            ],
        )
        .await
        .map(|_| ())
    }

    pub async fn update_in_tx<T: DatabaseTransaction>(tx: &T, issue: &Issue) -> Result<(), PersistenceError> {
        tx.execute_params(
            SQL_UPDATE,
            &rume::params![
                &issue.status,
                &issue.priority,
                &issue.assignee_id,
                issue.updated_at,
                &issue.id,
                &issue.tenant_id,
            ],
        )
        .await
        .map(|_| ())
    }

    pub async fn find(
        pool: &DatabasePool<PostgresBackend>,
        id: &str,
        tenant_id: &str,
    ) -> Result<Option<Issue>, PersistenceError> {
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

    pub async fn list_by_project(
        pool: &DatabasePool<PostgresBackend>,
        project_id: &str,
        tenant_id: &str,
    ) -> Result<Vec<Issue>, PersistenceError> {
        let rows = sqlx::query(SQL_LIST)
            .bind(project_id)
            .bind(tenant_id)
            .fetch_all(pool.backend().pool())
            .await
            .map_err(|e| PersistenceError::QueryFailed(e.to_string()))?;

        let mut issues = Vec::with_capacity(rows.len());
        for row in rows {
            issues.push(Self::map_row(&row)?);
        }
        Ok(issues)
    }

    fn map_row(row: &sqlx::postgres::PgRow) -> Result<Issue, PersistenceError> {
        Ok(Issue {
            id: row.try_get("id").map_err(|e| PersistenceError::QueryFailed(e.to_string()))?,
            tenant_id: row.try_get("tenant_id").map_err(|e| PersistenceError::QueryFailed(e.to_string()))?,
            project_id: row.try_get("project_id").map_err(|e| PersistenceError::QueryFailed(e.to_string()))?,
            title: row.try_get("title").map_err(|e| PersistenceError::QueryFailed(e.to_string()))?,
            description: row.try_get("description").map_err(|e| PersistenceError::QueryFailed(e.to_string()))?,
            status: row.try_get("status").map_err(|e| PersistenceError::QueryFailed(e.to_string()))?,
            priority: row.try_get("priority").map_err(|e| PersistenceError::QueryFailed(e.to_string()))?,
            assignee_id: row.try_get("assignee_id").map_err(|e| PersistenceError::QueryFailed(e.to_string()))?,
            created_at: row.try_get("created_at").map_err(|e| PersistenceError::QueryFailed(e.to_string()))?,
            updated_at: row.try_get("updated_at").map_err(|e| PersistenceError::QueryFailed(e.to_string()))?,
        })
    }
}
