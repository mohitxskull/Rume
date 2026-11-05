use rume::persistence::error::PersistenceError;
use rume::persistence::pool::DatabasePool;
use rume::persistence::postgres::PostgresBackend;

use super::queries;
use crate::models::Project;

pub struct ProjectRepository;

impl ProjectRepository {
    pub async fn create(
        pool: &DatabasePool<PostgresBackend>,
        project: &Project,
    ) -> Result<(), PersistenceError> {
        queries::projects::CREATE
            .execute_pool(
                pool,
                &rume::params![
                    &project.id,
                    &project.tenant_id,
                    &project.name,
                    &project.description,
                    project.created_at,
                ],
            )
            .await
            .map(|_| ())
    }

    pub async fn find(
        pool: &DatabasePool<PostgresBackend>,
        id: &str,
        tenant_id: &str,
    ) -> Result<Option<Project>, PersistenceError> {
        queries::projects::FIND
            .fetch_optional_as(pool, &rume::params![id, tenant_id])
            .await
    }

    pub async fn list_by_tenant(
        pool: &DatabasePool<PostgresBackend>,
        tenant_id: &str,
    ) -> Result<Vec<Project>, PersistenceError> {
        queries::projects::LIST
            .fetch_all_as(pool, &rume::params![tenant_id])
            .await
    }
}
