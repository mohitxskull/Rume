use rume::persistence::backend::DatabaseTransaction;
use rume::persistence::error::PersistenceError;
use rume::persistence::pool::DatabasePool;
use rume::persistence::postgres::PostgresBackend;

use super::queries;
use crate::models::Issue;

pub struct IssueRepository;

impl IssueRepository {
    pub async fn create_in_tx<T: DatabaseTransaction>(
        tx: &T,
        issue: &Issue,
    ) -> Result<(), PersistenceError> {
        queries::issues::CREATE
            .execute(
                tx,
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

    pub async fn update_in_tx<T: DatabaseTransaction>(
        tx: &T,
        issue: &Issue,
    ) -> Result<(), PersistenceError> {
        queries::issues::UPDATE
            .execute(
                tx,
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
        queries::issues::FIND
            .fetch_optional_as(pool, &rume::params![id, tenant_id])
            .await
    }

    pub async fn list_by_project(
        pool: &DatabasePool<PostgresBackend>,
        project_id: &str,
        tenant_id: &str,
    ) -> Result<Vec<Issue>, PersistenceError> {
        queries::issues::LIST
            .fetch_all_as(pool, &rume::params![project_id, tenant_id])
            .await
    }
}
