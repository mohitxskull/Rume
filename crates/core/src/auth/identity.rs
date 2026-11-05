//! Authenticated identity representation and extractors.

use std::collections::BTreeMap;
use std::ops::{Deref, DerefMut};

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use serde::{Deserialize, Serialize};

use crate::error::AppError;

/// Core trait representing an authenticated subject.
pub trait Identity: Send + Sync + 'static {
    /// Unique subject or user identifier.
    fn id(&self) -> &str;

    /// Roles granted to this identity.
    fn roles(&self) -> &[String];

    /// Permissions granted to this identity.
    fn permissions(&self) -> &[String];
}

/// Standard identity model representing an authenticated user, service account, or token subject.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthUser {
    /// Unique user or subject identifier.
    pub id: String,
    /// Optional tenant or organization identifier to enforce multi-tenant authorization boundaries.
    pub tenant_id: Option<String>,
    /// Roles assigned to this user (e.g., "admin", "editor", "member").
    pub roles: Vec<String>,
    /// Fine-grained permission strings (e.g., "users:read", "billing:write").
    pub permissions: Vec<String>,
    /// Additional contextual metadata.
    pub metadata: BTreeMap<String, String>,
}

impl AuthUser {
    /// Creates a new user with the given identifier.
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            tenant_id: None,
            roles: Vec::new(),
            permissions: Vec::new(),
            metadata: BTreeMap::new(),
        }
    }

    /// Binds the user to a specific tenant or organization identifier.
    pub fn with_tenant_id(mut self, tenant_id: impl Into<String>) -> Self {
        self.tenant_id = Some(tenant_id.into());
        self
    }

    /// Adds a metadata key-value pair to the user.
    pub fn with_metadata(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.metadata.insert(key.into(), value.into());
        self
    }

    /// Adds a role to the user.
    pub fn with_role(mut self, role: impl Into<String>) -> Self {
        self.roles.push(role.into());
        self
    }

    /// Adds multiple roles to the user.
    pub fn with_roles(mut self, roles: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.roles.extend(roles.into_iter().map(Into::into));
        self
    }

    /// Adds a permission to the user.
    pub fn with_permission(mut self, permission: impl Into<String>) -> Self {
        self.permissions.push(permission.into());
        self
    }

    /// Adds multiple permissions to the user.
    pub fn with_permissions(mut self, perms: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.permissions.extend(perms.into_iter().map(Into::into));
        self
    }

    /// Checks if the user has a specific role.
    pub fn has_role(&self, role: &str) -> bool {
        self.roles.iter().any(|r| r == role)
    }

    /// Checks if the user has any of the specified roles.
    pub fn has_any_role(&self, roles: &[&str]) -> bool {
        roles.iter().any(|r| self.has_role(r))
    }

    /// Checks if the user has a specific permission.
    pub fn has_permission(&self, perm: &str) -> bool {
        self.permissions.iter().any(|p| p == perm)
    }

    /// Checks if the user has all of the specified permissions.
    pub fn has_all_permissions(&self, perms: &[&str]) -> bool {
        perms.iter().all(|p| self.has_permission(p))
    }
}

impl Identity for AuthUser {
    fn id(&self) -> &str {
        &self.id
    }

    fn roles(&self) -> &[String] {
        &self.roles
    }

    fn permissions(&self) -> &[String] {
        &self.permissions
    }
}

/// An extractor that requires an authenticated identity to be present in request extensions.
///
/// If an authenticated identity is found, the handler receives it.
/// If not present, the request is rejected with [`AppError::Unauthorized`] (RFC 9457 401 Problem Details).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Authenticated<T = AuthUser>(pub T);

impl<T> Authenticated<T> {
    /// Consumes the wrapper and returns the inner identity.
    pub fn into_inner(self) -> T {
        self.0
    }
}

impl<T> Deref for Authenticated<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T> DerefMut for Authenticated<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<T: Clone + Send + Sync + 'static, S: Send + Sync> FromRequestParts<S> for Authenticated<T> {
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<T>()
            .cloned()
            .map(Authenticated)
            .ok_or(AppError::Unauthorized)
    }
}

/// An extractor that optionally retrieves an authenticated identity from request extensions.
///
/// Useful for public endpoints that provide personalized behavior when a user is logged in
/// (e.g. guest vs authenticated user).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OptionalAuth<T = AuthUser>(pub Option<T>);

impl<T> OptionalAuth<T> {
    /// Consumes the wrapper and returns the optional inner identity.
    pub fn into_inner(self) -> Option<T> {
        self.0
    }
}

impl<T> Deref for OptionalAuth<T> {
    type Target = Option<T>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T> DerefMut for OptionalAuth<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<T: Clone + Send + Sync + 'static, S: Send + Sync> FromRequestParts<S> for OptionalAuth<T> {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let auth = parts.extensions.get::<T>().cloned();
        Ok(OptionalAuth(auth))
    }
}
