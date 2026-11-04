use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use rume::auth::identity::AuthUser;
use rume::auth::Authenticated;
use rume::error::AppError;
use rume::tenant::TenantContext;
use sqlx::Row;

use crate::models::{CreateUserRequest, User};
use crate::state::WorkbenchState;

pub async fn create_user(
    TenantContext(tenant): TenantContext,
    State(state): State<WorkbenchState>,
    Json(payload): Json<CreateUserRequest>,
) -> Result<(StatusCode, Json<User>), AppError> {
    let id = payload.id.trim();
    let email = payload.email.trim();
    let name = payload.name.trim();
    let role = payload.role.trim();
    let token = payload.api_token.trim();

    if id.is_empty() || email.is_empty() || name.is_empty() || role.is_empty() || token.is_empty() {
        return Err(AppError::bad_request("All user fields are required"));
    }

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;

    let user = User {
        id: id.to_string(),
        organization_id: tenant.id.clone(),
        email: email.to_string(),
        name: name.to_string(),
        role: role.to_string(),
        api_token: token.to_string(),
        created_at: now,
    };

    let sql = r#"
        INSERT INTO workbench_users (id, organization_id, email, name, role, api_token, created_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7)
    "#;

    sqlx::query(sql)
        .bind(&user.id)
        .bind(&user.organization_id)
        .bind(&user.email)
        .bind(&user.name)
        .bind(&user.role)
        .bind(&user.api_token)
        .bind(user.created_at)
        .execute(state.pool.backend().pool())
        .await
        .map_err(|e| AppError::internal(format!("Failed to create user: {e}")))?;

    Ok((StatusCode::CREATED, Json(user)))
}

pub async fn list_users(
    TenantContext(tenant): TenantContext,
    State(state): State<WorkbenchState>,
) -> Result<Json<Vec<User>>, AppError> {
    let sql = r#"
        SELECT id, organization_id, email, name, role, api_token, created_at
        FROM workbench_users
        WHERE organization_id = $1
        ORDER BY name ASC
    "#;
    let rows = sqlx::query(sql)
        .bind(&tenant.id)
        .fetch_all(state.pool.backend().pool())
        .await
        .map_err(|e| AppError::internal(format!("Database error: {e}")))?;

    let mut users = Vec::with_capacity(rows.len());
    for row in rows {
        users.push(User {
            id: row.try_get("id").map_err(|e| AppError::internal(e.to_string()))?,
            organization_id: row.try_get("organization_id").map_err(|e| AppError::internal(e.to_string()))?,
            email: row.try_get("email").map_err(|e| AppError::internal(e.to_string()))?,
            name: row.try_get("name").map_err(|e| AppError::internal(e.to_string()))?,
            role: row.try_get("role").map_err(|e| AppError::internal(e.to_string()))?,
            api_token: row.try_get("api_token").map_err(|e| AppError::internal(e.to_string()))?,
            created_at: row.try_get("created_at").map_err(|e| AppError::internal(e.to_string()))?,
        });
    }

    Ok(Json(users))
}

pub async fn get_user(
    TenantContext(tenant): TenantContext,
    Path(id): Path<String>,
    State(state): State<WorkbenchState>,
) -> Result<Json<User>, AppError> {
    let sql = r#"
        SELECT id, organization_id, email, name, role, api_token, created_at
        FROM workbench_users
        WHERE id = $1 AND organization_id = $2
    "#;
    let row = sqlx::query(sql)
        .bind(&id)
        .bind(&tenant.id)
        .fetch_optional(state.pool.backend().pool())
        .await
        .map_err(|e| AppError::internal(format!("Database error: {e}")))?
        .ok_or_else(|| AppError::not_found(format!("User '{id}' not found in organization")))?;

    Ok(Json(User {
        id: row.try_get("id").map_err(|e| AppError::internal(e.to_string()))?,
        organization_id: row.try_get("organization_id").map_err(|e| AppError::internal(e.to_string()))?,
        email: row.try_get("email").map_err(|e| AppError::internal(e.to_string()))?,
        name: row.try_get("name").map_err(|e| AppError::internal(e.to_string()))?,
        role: row.try_get("role").map_err(|e| AppError::internal(e.to_string()))?,
        api_token: row.try_get("api_token").map_err(|e| AppError::internal(e.to_string()))?,
        created_at: row.try_get("created_at").map_err(|e| AppError::internal(e.to_string()))?,
    }))
}

pub async fn get_current_user(
    Authenticated(user): Authenticated<AuthUser>,
) -> Result<Json<AuthUser>, AppError> {
    Ok(Json(user))
}
