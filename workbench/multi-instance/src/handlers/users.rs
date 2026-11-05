use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use rume::auth::Authenticated;
use rume::auth::identity::AuthUser;
use rume::error::AppError;
use rume::tenant::TenantContext;

use crate::models::{CreateUserRequest, User};
use crate::repositories::UserRepository;
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

    UserRepository::create(&state.pool, &user)
        .await
        .map_err(|e| AppError::internal(format!("Failed to create user: {e}")))?;

    Ok((StatusCode::CREATED, Json(user)))
}

pub async fn list_users(
    TenantContext(tenant): TenantContext,
    State(state): State<WorkbenchState>,
) -> Result<Json<Vec<User>>, AppError> {
    let users = UserRepository::list_by_organization(&state.pool, &tenant.id)
        .await
        .map_err(|e| AppError::internal(format!("Database error: {e}")))?;

    Ok(Json(users))
}

pub async fn get_user(
    TenantContext(tenant): TenantContext,
    Path(id): Path<String>,
    State(state): State<WorkbenchState>,
) -> Result<Json<User>, AppError> {
    let user = UserRepository::find(&state.pool, &id, &tenant.id)
        .await
        .map_err(|e| AppError::internal(format!("Database error: {e}")))?
        .ok_or_else(|| AppError::not_found(format!("User '{id}' not found in organization")))?;

    Ok(Json(user))
}

pub async fn get_current_user(
    Authenticated(user): Authenticated<AuthUser>,
) -> Result<Json<AuthUser>, AppError> {
    Ok(Json(user))
}
