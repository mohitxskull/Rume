use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use rume::error::AppError;
use sqlx::Row;

use crate::models::{CreateOrganizationRequest, Organization};
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

    let sql = "INSERT INTO workbench_organizations (id, name, created_at) VALUES ($1, $2, $3)";
    sqlx::query(sql)
        .bind(&org.id)
        .bind(&org.name)
        .bind(org.created_at)
        .execute(state.pool.backend().pool())
        .await
        .map_err(|e| AppError::internal(format!("Failed to create organization: {e}")))?;

    Ok((StatusCode::CREATED, Json(org)))
}

pub async fn get_organization(
    Path(id): Path<String>,
    State(state): State<WorkbenchState>,
) -> Result<Json<Organization>, AppError> {
    let sql = "SELECT id, name, created_at FROM workbench_organizations WHERE id = $1";
    let row = sqlx::query(sql)
        .bind(&id)
        .fetch_optional(state.pool.backend().pool())
        .await
        .map_err(|e| AppError::internal(format!("Database error: {e}")))?
        .ok_or_else(|| AppError::not_found(format!("Organization '{id}' not found")))?;

    Ok(Json(Organization {
        id: row.try_get("id").map_err(|e| AppError::internal(e.to_string()))?,
        name: row.try_get("name").map_err(|e| AppError::internal(e.to_string()))?,
        created_at: row.try_get("created_at").map_err(|e| AppError::internal(e.to_string()))?,
    }))
}

pub async fn list_organizations(
    State(state): State<WorkbenchState>,
) -> Result<Json<Vec<Organization>>, AppError> {
    let sql = "SELECT id, name, created_at FROM workbench_organizations ORDER BY name ASC";
    let rows = sqlx::query(sql)
        .fetch_all(state.pool.backend().pool())
        .await
        .map_err(|e| AppError::internal(format!("Database error: {e}")))?;

    let mut orgs = Vec::with_capacity(rows.len());
    for row in rows {
        orgs.push(Organization {
            id: row.try_get("id").map_err(|e| AppError::internal(e.to_string()))?,
            name: row.try_get("name").map_err(|e| AppError::internal(e.to_string()))?,
            created_at: row.try_get("created_at").map_err(|e| AppError::internal(e.to_string()))?,
        });
    }

    Ok(Json(orgs))
}
