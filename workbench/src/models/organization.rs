use rume::persistence::error::PersistenceError;
use rume::persistence::row::{DbRow, FromRow};
use serde::{Deserialize, Serialize};

/// Persisted organization (tenant root) entity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Organization {
    pub id: String,
    pub name: String,
    pub created_at: i64,
}

impl FromRow for Organization {
    fn from_row(row: &DbRow) -> Result<Self, PersistenceError> {
        Ok(Self {
            id: row.get("id")?,
            name: row.get("name")?,
            created_at: row.get("created_at")?,
        })
    }
}

/// Request payload to create a new organization.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateOrganizationRequest {
    pub id: String,
    pub name: String,
}
