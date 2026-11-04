use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use rume::error::AppError;

use crate::models::{CreateOrganizationRequest, Organization};
use crate::repositories::OrganizationRepository;
use crate::state::WorkbenchState;

pub async fn create_organization(
    State(state): State<WorkbenchState>,
    Json(payload): Json<CreateOrganizationRequest>,
) -> Result<(StatusCode, Json<Organization>), AppError> {
    let id = payload.id.trim();
    let name = payload.name.trim();
    if id.is_empty() || name.is_empty() {
        return Err(AppError::bad_request("Organization id and name cannot be empty"));
    }

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;

    let org = Organization {
        id: id.to_string(),
        name: name.to_string(),
        created_at: now,
    };

    OrganizationRepository::create(&state.pool, &org)
        .await
        .map_err(|e| AppError::internal(format!("Failed to create organization: {e}")))?;

    Ok((StatusCode::CREATED, Json(org)))
}

pub async fn get_organization(
    Path(id): Path<String>,
    State(state): State<WorkbenchState>,
) -> Result<Json<Organization>, AppError> {
    let org = OrganizationRepository::find(&state.pool, &id)
        .await
        .map_err(|e| AppError::internal(format!("Database error: {e}")))?
        .ok_or_else(|| AppError::not_found(format!("Organization '{id}' not found")))?;

    Ok(Json(org))
}

pub async fn list_organizations(
    State(state): State<WorkbenchState>,
) -> Result<Json<Vec<Organization>>, AppError> {
    let orgs = OrganizationRepository::list_all(&state.pool)
        .await
        .map_err(|e| AppError::internal(format!("Database error: {e}")))?;

    Ok(Json(orgs))
}
