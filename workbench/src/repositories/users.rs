use rume::auth::identity::AuthUser;
use rume::persistence::error::PersistenceError;
use rume::persistence::pool::DatabasePool;
use rume::persistence::postgres::PostgresBackend;
use sqlx::Row;

use crate::models::User;

const SQL_CREATE: &str = include_str!("../../sql/postgres/users/create.sql");
const SQL_LIST: &str = include_str!("../../sql/postgres/users/list.sql");
const SQL_FIND: &str = include_str!("../../sql/postgres/users/find.sql");
const SQL_FIND_BY_TOKEN: &str = include_str!("../../sql/postgres/users/find_by_token.sql");

pub struct UserRepository;

impl UserRepository {
    pub async fn create(
        pool: &DatabasePool<PostgresBackend>,
        user: &User,
    ) -> Result<(), PersistenceError> {
        sqlx::query(SQL_CREATE)
            .bind(&user.id)
            .bind(&user.organization_id)
            .bind(&user.email)
            .bind(&user.name)
            .bind(&user.role)
            .bind(&user.api_token)
            .bind(user.created_at)
            .execute(pool.backend().pool())
            .await
            .map_err(|e| PersistenceError::QueryFailed(e.to_string()))?;
        Ok(())
    }

    pub async fn find(
        pool: &DatabasePool<PostgresBackend>,
        id: &str,
        organization_id: &str,
    ) -> Result<Option<User>, PersistenceError> {
        let row = sqlx::query(SQL_FIND)
            .bind(id)
            .bind(organization_id)
            .fetch_optional(pool.backend().pool())
            .await
            .map_err(|e| PersistenceError::QueryFailed(e.to_string()))?;

        match row {
            Some(r) => Ok(Some(Self::map_row(&r)?)),
            None => Ok(None),
        }
    }

    pub async fn list_by_organization(
        pool: &DatabasePool<PostgresBackend>,
        organization_id: &str,
    ) -> Result<Vec<User>, PersistenceError> {
        let rows = sqlx::query(SQL_LIST)
            .bind(organization_id)
            .fetch_all(pool.backend().pool())
            .await
            .map_err(|e| PersistenceError::QueryFailed(e.to_string()))?;

        let mut users = Vec::with_capacity(rows.len());
        for row in rows {
            users.push(Self::map_row(&row)?);
        }
        Ok(users)
    }

    pub async fn find_by_token(
        pool: &DatabasePool<PostgresBackend>,
        token: &str,
    ) -> Result<Option<AuthUser>, PersistenceError> {
        let row = sqlx::query(SQL_FIND_BY_TOKEN)
            .bind(token)
            .fetch_optional(pool.backend().pool())
            .await
            .map_err(|e| PersistenceError::QueryFailed(e.to_string()))?;

        if let Some(r) = row {
            let id: String = r
                .try_get("id")
                .map_err(|e| PersistenceError::QueryFailed(e.to_string()))?;
            let org_id: String = r
                .try_get("organization_id")
                .map_err(|e| PersistenceError::QueryFailed(e.to_string()))?;
            let role: String = r
                .try_get("role")
                .map_err(|e| PersistenceError::QueryFailed(e.to_string()))?;
            let name: String = r
                .try_get("name")
                .map_err(|e| PersistenceError::QueryFailed(e.to_string()))?;
            let email: String = r
                .try_get("email")
                .map_err(|e| PersistenceError::QueryFailed(e.to_string()))?;

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

    fn map_row(row: &sqlx::postgres::PgRow) -> Result<User, PersistenceError> {
        Ok(User {
            id: row.try_get("id").map_err(|e| PersistenceError::QueryFailed(e.to_string()))?,
            organization_id: row.try_get("organization_id").map_err(|e| PersistenceError::QueryFailed(e.to_string()))?,
            email: row.try_get("email").map_err(|e| PersistenceError::QueryFailed(e.to_string()))?,
            name: row.try_get("name").map_err(|e| PersistenceError::QueryFailed(e.to_string()))?,
            role: row.try_get("role").map_err(|e| PersistenceError::QueryFailed(e.to_string()))?,
            api_token: row.try_get("api_token").map_err(|e| PersistenceError::QueryFailed(e.to_string()))?,
            created_at: row.try_get("created_at").map_err(|e| PersistenceError::QueryFailed(e.to_string()))?,
        })
    }
}
