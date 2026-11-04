use std::sync::Arc;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use rume::error::AppError;
use rume::outbox::{OutboxMessage, TransactionalOutbox};
use rume::persistence::backend::DatabaseTransaction;
use rume::persistence::error::PersistenceError;
use rume::tenant::TenantContext;
use sqlx::Row;
use uuid::Uuid;

use crate::models::{
    CreateIssueRequest, Issue, IssueCreatedEvent, IssueUpdatedEvent, UpdateIssueRequest,
};
use crate::state::WorkbenchState;

const VALID_STATUSES: &[&str] = &["open", "in_progress", "resolved", "closed"];
const VALID_PRIORITIES: &[&str] = &["low", "medium", "high", "urgent"];

pub async fn create_issue(
    TenantContext(tenant): TenantContext,
    Path(project_id): Path<String>,
    State(state): State<WorkbenchState>,
    Json(payload): Json<CreateIssueRequest>,
) -> Result<(StatusCode, Json<Issue>), AppError> {
    let title = payload.title.trim();
    if title.is_empty() {
        return Err(AppError::bad_request("Issue title cannot be empty"));
    }
    if title.len() > 200 {
        return Err(AppError::bad_request(
            "Issue title exceeds maximum length of 200 characters",
        ));
    }

    let status = payload.status.to_lowercase();
    if !VALID_STATUSES.contains(&status.as_str()) {
        return Err(AppError::bad_request(format!(
            "Invalid status '{status}'. Must be one of: open, in_progress, resolved, closed"
        )));
    }

    let priority = payload.priority.to_lowercase();
    if !VALID_PRIORITIES.contains(&priority.as_str()) {
        return Err(AppError::bad_request(format!(
            "Invalid priority '{priority}'. Must be one of: low, medium, high, urgent"
        )));
    }

    // Verify project exists for this tenant
    let project_check = sqlx::query("SELECT id FROM workbench_projects WHERE id = $1 AND tenant_id = $2")
        .bind(&project_id)
        .bind(&tenant.id)
        .fetch_optional(state.pool.backend().pool())
        .await
        .map_err(|e| AppError::internal(format!("Database query error: {e}")))?;

    if project_check.is_none() {
        return Err(AppError::not_found(format!(
            "Project '{project_id}' not found"
        )));
    }

    let issue_id = format!("issue-{}", Uuid::new_v4());
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;

    let issue = Issue {
        id: issue_id.clone(),
        tenant_id: tenant.id.clone(),
        project_id: project_id.clone(),
        title: title.to_string(),
        description: payload.description.clone(),
        status,
        priority,
        assignee_id: payload.assignee_id.clone(),
        created_at: now,
        updated_at: now,
    };

    // Prepare outbox event for atomic publication
    let event = IssueCreatedEvent {
        tenant_id: tenant.id.clone(),
        project_id: project_id.clone(),
        issue_id: issue_id.clone(),
        title: issue.title.clone(),
        assignee_id: issue.assignee_id.clone(),
    };
    let outbox_msg = OutboxMessage::from_event(&event)
        .map_err(|e| AppError::internal(format!("Serialization error: {e}")))?;

    let pool_clone = Arc::new(state.pool.clone());
    let issue_clone = issue.clone();

    // Atomic Database Transaction: Entity write + Outbox enrollment
    state
        .pool
        .transaction(move |tx| {
            let pool = Arc::clone(&pool_clone);
            let iss = issue_clone.clone();
            let msg = outbox_msg.clone();
            async move {
                let sql = format!(
                    "INSERT INTO workbench_issues (id, tenant_id, project_id, title, description, status, priority, assignee_id, created_at, updated_at) VALUES ('{}', '{}', '{}', '{}', {}, '{}', '{}', {}, {}, {});",
                    iss.id,
                    iss.tenant_id,
                    iss.project_id,
                    iss.title.replace('\'', "''"),
                    iss.description.as_ref().map(|d| format!("'{}'", d.replace('\'', "''"))).unwrap_or_else(|| "NULL".to_string()),
                    iss.status,
                    iss.priority,
                    iss.assignee_id.as_ref().map(|a| format!("'{}'", a.replace('\'', "''"))).unwrap_or_else(|| "NULL".to_string()),
                    iss.created_at,
                    iss.updated_at
                );
                tx.execute(&sql).await?;
                pool.save_tx(&tx, msg)
                    .await
                    .map_err(|e| PersistenceError::QueryFailed(e.to_string()))?;
                Ok::<(), PersistenceError>(())
            }
        })
        .await
        .map_err(|e| AppError::internal(format!("Failed to create issue in transaction: {e}")))?;

    Ok((StatusCode::CREATED, Json(issue)))
}

pub async fn list_issues(
    TenantContext(tenant): TenantContext,
    Path(project_id): Path<String>,
    State(state): State<WorkbenchState>,
) -> Result<Json<Vec<Issue>>, AppError> {
    let sql = "SELECT id, tenant_id, project_id, title, description, status, priority, assignee_id, created_at, updated_at FROM workbench_issues WHERE project_id = $1 AND tenant_id = $2 ORDER BY created_at ASC";
    let rows = sqlx::query(sql)
        .bind(&project_id)
        .bind(&tenant.id)
        .fetch_all(state.pool.backend().pool())
        .await
        .map_err(|e| AppError::internal(format!("Failed to list issues: {e}")))?;

    let mut issues = Vec::with_capacity(rows.len());
    for row in rows {
        issues.push(Issue {
            id: row.get("id"),
            tenant_id: row.get("tenant_id"),
            project_id: row.get("project_id"),
            title: row.get("title"),
            description: row.get("description"),
            status: row.get("status"),
            priority: row.get("priority"),
            assignee_id: row.get("assignee_id"),
            created_at: row.get("created_at"),
            updated_at: row.get("updated_at"),
        });
    }

    Ok(Json(issues))
}

pub async fn get_issue(
    TenantContext(tenant): TenantContext,
    Path(id): Path<String>,
    State(state): State<WorkbenchState>,
) -> Result<Json<Issue>, AppError> {
    let sql = "SELECT id, tenant_id, project_id, title, description, status, priority, assignee_id, created_at, updated_at FROM workbench_issues WHERE id = $1 AND tenant_id = $2";
    let row = sqlx::query(sql)
        .bind(&id)
        .bind(&tenant.id)
        .fetch_optional(state.pool.backend().pool())
        .await
        .map_err(|e| AppError::internal(format!("Failed to get issue: {e}")))?;

    match row {
        Some(r) => Ok(Json(Issue {
            id: r.get("id"),
            tenant_id: r.get("tenant_id"),
            project_id: r.get("project_id"),
            title: r.get("title"),
            description: r.get("description"),
            status: r.get("status"),
            priority: r.get("priority"),
            assignee_id: r.get("assignee_id"),
            created_at: r.get("created_at"),
            updated_at: r.get("updated_at"),
        })),
        None => Err(AppError::not_found(format!("Issue '{id}' not found"))),
    }
}

pub async fn update_issue(
    TenantContext(tenant): TenantContext,
    Path(id): Path<String>,
    State(state): State<WorkbenchState>,
    Json(payload): Json<UpdateIssueRequest>,
) -> Result<Json<Issue>, AppError> {
    let mut current = get_issue(TenantContext(tenant.clone()), Path(id.clone()), State(state.clone()))
        .await?
        .0;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;

    if let Some(s) = &payload.status {
        let status = s.to_lowercase();
        if !VALID_STATUSES.contains(&status.as_str()) {
            return Err(AppError::bad_request("Invalid status"));
        }
        current.status = status;
    }
    if let Some(p) = &payload.priority {
        let priority = p.to_lowercase();
        if !VALID_PRIORITIES.contains(&priority.as_str()) {
            return Err(AppError::bad_request("Invalid priority"));
        }
        current.priority = priority;
    }
    if let Some(a) = &payload.assignee_id {
        current.assignee_id = Some(a.clone());
    }
    current.updated_at = now;

    let event = IssueUpdatedEvent {
        tenant_id: tenant.id.clone(),
        issue_id: id.clone(),
        status: payload.status.clone(),
        priority: payload.priority.clone(),
    };
    let outbox_msg = OutboxMessage::from_event(&event)
        .map_err(|e| AppError::internal(format!("Serialization error: {e}")))?;

    let pool_clone = Arc::new(state.pool.clone());
    let iss = current.clone();

    state
        .pool
        .transaction(move |tx| {
            let pool = Arc::clone(&pool_clone);
            let iss = iss.clone();
            let msg = outbox_msg.clone();
            async move {
                let sql = format!(
                    "UPDATE workbench_issues SET status = '{}', priority = '{}', assignee_id = {}, updated_at = {} WHERE id = '{}' AND tenant_id = '{}';",
                    iss.status,
                    iss.priority,
                    iss.assignee_id.as_ref().map(|a| format!("'{}'", a.replace('\'', "''"))).unwrap_or_else(|| "NULL".to_string()),
                    iss.updated_at,
                    iss.id,
                    iss.tenant_id
                );
                tx.execute(&sql).await?;
                pool.save_tx(&tx, msg)
                    .await
                    .map_err(|e| PersistenceError::QueryFailed(e.to_string()))?;
                Ok::<(), PersistenceError>(())
            }
        })
        .await
        .map_err(|e| AppError::internal(format!("Failed to update issue in transaction: {e}")))?;

    Ok(Json(current))
}
