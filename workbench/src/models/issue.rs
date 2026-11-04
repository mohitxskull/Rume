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
