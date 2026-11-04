use rume::events::Event;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssueCreatedEvent {
    pub tenant_id: String,
    pub project_id: String,
    pub issue_id: String,
    pub title: String,
    pub assignee_id: Option<String>,
}

impl Event for IssueCreatedEvent {
    const TYPE: &'static str = "issue.created";
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssueUpdatedEvent {
    pub tenant_id: String,
    pub issue_id: String,
    pub status: Option<String>,
    pub priority: Option<String>,
}

impl Event for IssueUpdatedEvent {
    const TYPE: &'static str = "issue.updated";
}
