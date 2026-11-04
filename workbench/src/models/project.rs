use rume::persistence::error::PersistenceError;
use rume::persistence::row::{DbRow, FromRow};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub tenant_id: String,
    pub name: String,
    pub description: Option<String>,
    pub created_at: i64,
}

impl FromRow for Project {
    fn from_row(row: &DbRow) -> Result<Self, PersistenceError> {
        Ok(Self {
            id: row.get("id")?,
            tenant_id: row.get("tenant_id")?,
            name: row.get("name")?,
            description: row.get_opt("description")?,
            created_at: row.get("created_at")?,
        })
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateProjectRequest {
    pub name: String,
    pub description: Option<String>,
}
