use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use rume::error::AppError;
use rume::tenant::TenantContext;
use sqlx::Row;
use uuid::Uuid;

use crate::models::{CreateProjectRequest, Project};
use crate::state::WorkbenchState;

pub async fn create_project(
    TenantContext(tenant): TenantContext,
    State(state): State<WorkbenchState>,
    Json(payload): Json<CreateProjectRequest>,
) -> Result<(StatusCode, Json<Project>), AppError> {
    let name = payload.name.trim();
    if name.is_empty() {
        return Err(AppError::bad_request("Project name cannot be empty"));
    }
    if name.len() > 100 {
        return Err(AppError::bad_request("Project name exceeds maximum length of 100 characters"));
    }

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

    let sql = "INSERT INTO workbench_projects (id, tenant_id, name, description, created_at) VALUES ($1, $2, $3, $4, $5)";
    sqlx::query(sql)
        .bind(&project.id)
        .bind(&project.tenant_id)
        .bind(&project.name)
        .bind(&project.description)
        .bind(project.created_at)
        .execute(state.pool.backend().pool())
        .await
        .map_err(|e| AppError::internal(format!("Failed to create project: {e}")))?;

    Ok((StatusCode::CREATED, Json(project)))
}

pub async fn list_projects(
    TenantContext(tenant): TenantContext,
    State(state): State<WorkbenchState>,
) -> Result<Json<Vec<Project>>, AppError> {
    let sql = "SELECT id, tenant_id, name, description, created_at FROM workbench_projects WHERE tenant_id = $1 ORDER BY created_at DESC";
    let rows = sqlx::query(sql)
        .bind(&tenant.id)
        .fetch_all(state.pool.backend().pool())
        .await
        .map_err(|e| AppError::internal(format!("Failed to list projects: {e}")))?;

    let mut projects = Vec::with_capacity(rows.len());
    for row in rows {
        projects.push(Project {
            id: row.get("id"),
            tenant_id: row.get("tenant_id"),
            name: row.get("name"),
            description: row.get("description"),
            created_at: row.get("created_at"),
        });
    }

    Ok(Json(projects))
}

pub async fn get_project(
    TenantContext(tenant): TenantContext,
    Path(id): Path<String>,
    State(state): State<WorkbenchState>,
) -> Result<Json<Project>, AppError> {
    let sql = "SELECT id, tenant_id, name, description, created_at FROM workbench_projects WHERE id = $1 AND tenant_id = $2";
    let row = sqlx::query(sql)
        .bind(&id)
        .bind(&tenant.id)
        .fetch_optional(state.pool.backend().pool())
        .await
        .map_err(|e| AppError::internal(format!("Failed to get project: {e}")))?;

    match row {
        Some(r) => Ok(Json(Project {
            id: r.get("id"),
            tenant_id: r.get("tenant_id"),
            name: r.get("name"),
            description: r.get("description"),
            created_at: r.get("created_at"),
        })),
        None => Err(AppError::not_found(format!("Project '{id}' not found"))),
    }
}
