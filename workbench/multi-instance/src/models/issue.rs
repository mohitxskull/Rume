use garde::Validate;
use rume::persistence::error::PersistenceError;
use rume::persistence::row::{DbRow, FromRow};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Issue {
    pub id: String,
    pub tenant_id: String,
    pub project_id: String,
    pub title: String,
    pub description: Option<String>,
    pub status: String,
    pub priority: String,
    pub assignee_id: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

impl FromRow for Issue {
    fn from_row(row: &DbRow) -> Result<Self, PersistenceError> {
        Ok(Self {
            id: row.get("id")?,
            tenant_id: row.get("tenant_id")?,
            project_id: row.get("project_id")?,
            title: row.get("title")?,
            description: row.get_opt("description")?,
            status: row.get("status")?,
            priority: row.get("priority")?,
            assignee_id: row.get_opt("assignee_id")?,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
        })
    }
}

fn validate_status(status: &str, _ctx: &()) -> garde::Result {
    match status.to_lowercase().as_str() {
        "open" | "in_progress" | "resolved" | "closed" => Ok(()),
        _ => Err(garde::Error::new(
            "status must be one of: open, in_progress, resolved, closed",
        )),
    }
}

fn validate_priority(priority: &str, _ctx: &()) -> garde::Result {
    match priority.to_lowercase().as_str() {
        "low" | "medium" | "high" | "urgent" => Ok(()),
        _ => Err(garde::Error::new(
            "priority must be one of: low, medium, high, urgent",
        )),
    }
}

fn validate_opt_status(status: &Option<String>, ctx: &()) -> garde::Result {
    if let Some(s) = status {
        validate_status(s, ctx)
    } else {
        Ok(())
    }
}

fn validate_opt_priority(priority: &Option<String>, ctx: &()) -> garde::Result {
    if let Some(p) = priority {
        validate_priority(p, ctx)
    } else {
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize, Validate)]
pub struct CreateIssueRequest {
    #[garde(length(min = 1, max = 200))]
    pub title: String,
    #[garde(skip)]
    pub description: Option<String>,
    #[serde(default = "default_status")]
    #[garde(custom(validate_status))]
    pub status: String,
    #[serde(default = "default_priority")]
    #[garde(custom(validate_priority))]
    pub priority: String,
    #[garde(skip)]
    pub assignee_id: Option<String>,
}

fn default_status() -> String {
    "open".to_string()
}

fn default_priority() -> String {
    "medium".to_string()
}

#[derive(Debug, Clone, Deserialize, Validate)]
pub struct UpdateIssueRequest {
    #[garde(custom(validate_opt_status))]
    pub status: Option<String>,
    #[garde(custom(validate_opt_priority))]
    pub priority: Option<String>,
    #[garde(skip)]
    pub assignee_id: Option<String>,
}
