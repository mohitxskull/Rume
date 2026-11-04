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

#[derive(Debug, Clone, Deserialize)]
pub struct CreateIssueRequest {
    pub title: String,
    pub description: Option<String>,
    #[serde(default = "default_status")]
    pub status: String,
    #[serde(default = "default_priority")]
    pub priority: String,
    pub assignee_id: Option<String>,
}

fn default_status() -> String {
    "open".to_string()
}

fn default_priority() -> String {
    "medium".to_string()
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateIssueRequest {
    pub status: Option<String>,
    pub priority: Option<String>,
    pub assignee_id: Option<String>,
}
