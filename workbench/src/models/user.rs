use serde::{Deserialize, Serialize};

/// Persisted user entity associated with an organization and roles.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub id: String,
    pub organization_id: String,
    pub email: String,
    pub name: String,
    pub role: String,
    pub api_token: String,
    pub created_at: i64,
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
