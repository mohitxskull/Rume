use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use rume::error::AppError;
use rume::tenant::TenantContext;
use rume::validation::ValidatedJson;
use uuid::Uuid;

use crate::models::{CreateProjectRequest, Project};
use crate::repositories::ProjectRepository;
use crate::state::WorkbenchState;

pub async fn create_project(
    TenantContext(tenant): TenantContext,
    State(state): State<WorkbenchState>,
    ValidatedJson(payload): ValidatedJson<CreateProjectRequest>,
) -> Result<(StatusCode, Json<Project>), AppError> {
    let name = payload.name.trim();

    let id = format!("proj-{}", Uuid::new_v4());
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;

    let project = Project {
        id: id.clone(),
        tenant_id: tenant.id.clone(),
        name: name.to_string(),
        description: payload.description.clone(),
        created_at: now,
    };

    ProjectRepository::create(&state.pool, &project)
        .await
        .map_err(|e| AppError::internal(format!("Failed to create project: {e}")))?;

    Ok((StatusCode::CREATED, Json(project)))
}

pub async fn list_projects(
    TenantContext(tenant): TenantContext,
    State(state): State<WorkbenchState>,
) -> Result<Json<Vec<Project>>, AppError> {
    let projects = ProjectRepository::list_by_tenant(&state.pool, &tenant.id)
        .await
        .map_err(|e| AppError::internal(format!("Failed to list projects: {e}")))?;

    Ok(Json(projects))
}

pub async fn get_project(
    TenantContext(tenant): TenantContext,
    Path(id): Path<String>,
    State(state): State<WorkbenchState>,
) -> Result<Json<Project>, AppError> {
    let project = ProjectRepository::find(&state.pool, &id, &tenant.id)
        .await
        .map_err(|e| AppError::internal(format!("Failed to get project: {e}")))?
        .ok_or_else(|| AppError::not_found(format!("Project '{id}' not found")))?;

    Ok(Json(project))
}
