use std::fmt;

use rume::persistence::error::PersistenceError;
use rume::persistence::row::{DbRow, FromRow};
use serde::{Deserialize, Serialize};

/// Persisted user entity associated with an organization and roles.
#[derive(Clone, Serialize, Deserialize)]
pub struct User {
    pub id: String,
    pub organization_id: String,
    pub email: String,
    pub name: String,
    pub role: String,
    #[serde(skip_serializing, default)]
    pub api_token: String,
    pub created_at: i64,
}

impl fmt::Debug for User {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("User")
            .field("id", &self.id)
            .field("organization_id", &self.organization_id)
            .field("email", &self.email)
            .field("name", &self.name)
            .field("role", &self.role)
            .field("api_token", &"[REDACTED]")
            .field("created_at", &self.created_at)
            .finish()
    }
}

impl FromRow for User {
    fn from_row(row: &DbRow) -> Result<Self, PersistenceError> {
        Ok(Self {
            id: row.get("id")?,
            organization_id: row.get("organization_id")?,
            email: row.get("email")?,
            name: row.get("name")?,
            role: row.get("role")?,
            api_token: row.get("api_token")?,
            created_at: row.get("created_at")?,
        })
    }
}

/// Request payload to create a new user.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateUserRequest {
    pub id: String,
    pub email: String,
    pub name: String,
    pub role: String,
    pub api_token: String,
}
