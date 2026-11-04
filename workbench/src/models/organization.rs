use serde::{Deserialize, Serialize};

/// Persisted organization (tenant root) entity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Organization {
    pub id: String,
    pub name: String,
    pub created_at: i64,
}

/// Request payload to create a new organization.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateOrganizationRequest {
    pub id: String,
    pub name: String,
}
