use rume::persistence::error::PersistenceError;
use rume::persistence::pool::DatabasePool;
use rume::persistence::postgres::PostgresBackend;

use crate::models::Organization;
use super::queries;

pub struct OrganizationRepository;

impl OrganizationRepository {
    pub async fn create(
        pool: &DatabasePool<PostgresBackend>,
        org: &Organization,
    ) -> Result<(), PersistenceError> {
        queries::organizations::CREATE
            .execute_pool(
                pool,
                &rume::params![
                    &org.id,
                    &org.name,
                    org.created_at,
                ],
            )
            .await
            .map(|_| ())
    }

    pub async fn find(
        pool: &DatabasePool<PostgresBackend>,
        id: &str,
    ) -> Result<Option<Organization>, PersistenceError> {
        queries::organizations::FIND
            .fetch_optional_as(pool, &rume::params![id])
            .await
    }

    pub async fn list_all(
        pool: &DatabasePool<PostgresBackend>,
    ) -> Result<Vec<Organization>, PersistenceError> {
        queries::organizations::LIST
            .fetch_all_as(pool, &[])
            .await
    }
}
