use rume::auth::identity::AuthUser;
use rume::persistence::error::PersistenceError;
use rume::persistence::pool::DatabasePool;
use rume::persistence::postgres::PostgresBackend;

use crate::models::User;
use super::queries;

pub struct UserRepository;

impl UserRepository {
    pub async fn create(
        pool: &DatabasePool<PostgresBackend>,
        user: &User,
    ) -> Result<(), PersistenceError> {
        queries::users::CREATE
            .execute_pool(
                pool,
                &rume::params![
                    &user.id,
                    &user.organization_id,
                    &user.email,
                    &user.name,
                    &user.role,
                    &user.api_token,
                    user.created_at,
                ],
            )
            .await
            .map(|_| ())
    }

    pub async fn find(
        pool: &DatabasePool<PostgresBackend>,
        id: &str,
        organization_id: &str,
    ) -> Result<Option<User>, PersistenceError> {
        queries::users::FIND
            .fetch_optional_as(pool, &rume::params![id, organization_id])
            .await
    }

    pub async fn list_by_organization(
        pool: &DatabasePool<PostgresBackend>,
        organization_id: &str,
    ) -> Result<Vec<User>, PersistenceError> {
        queries::users::LIST
            .fetch_all_as(pool, &rume::params![organization_id])
            .await
    }

    pub async fn find_by_token(
        pool: &DatabasePool<PostgresBackend>,
        token: &str,
    ) -> Result<Option<AuthUser>, PersistenceError> {
        let row = queries::users::FIND_BY_TOKEN
            .fetch_optional(pool, &rume::params![token])
            .await?;

        if let Some(r) = row {
            let id: String = r.get("id")?;
            let org_id: String = r.get("organization_id")?;
            let role: String = r.get("role")?;
            let name: String = r.get("name")?;
            let email: String = r.get("email")?;

            let mut user = AuthUser::new(&id)
                .with_tenant_id(&org_id)
                .with_role(&role)
                .with_metadata("name", name)
                .with_metadata("email", email)
                .with_metadata("organization_id", org_id);

            if role == "admin" || role == "member" {
                user = user
                    .with_permission("projects:write")
                    .with_permission("issues:write");
            }
            if role == "viewer" || role == "member" || role == "admin" {
                user = user
                    .with_permission("projects:read")
                    .with_permission("issues:read");
            }

            Ok(Some(user))
        } else {
            Ok(None)
        }
    }
}
