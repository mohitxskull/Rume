//! Multi-tenant model and request extractors.

use std::collections::BTreeMap;
use std::ops::{Deref, DerefMut};

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use serde::{Deserialize, Serialize};

use crate::error::AppError;
use crate::tenant::error::TenantError;

/// An identified tenant in a multi-tenant application.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tenant {
    /// Unique tenant identifier (e.g. UUID).
    pub id: String,
    /// Human-friendly URL slug or subdomain (e.g. "acme").
    pub slug: String,
    /// Arbitrary tenant metadata.
    pub metadata: BTreeMap<String, String>,
}

impl Tenant {
    /// Creates a new tenant with ID and slug.
    pub fn new(id: impl Into<String>, slug: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            slug: slug.into(),
            metadata: BTreeMap::new(),
        }
    }
}

/// An extractor that requires an identified tenant in request extensions.
///
/// If a tenant was resolved, hands it to the handler.
/// If no tenant is present, rejects with [`AppError::BadRequest`] (RFC 9457 Problem Details).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TenantContext<T = Tenant>(pub T);

impl<T> TenantContext<T> {
    pub fn into_inner(self) -> T {
        self.0
    }
}

impl<T> Deref for TenantContext<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T> DerefMut for TenantContext<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<T: Clone + Send + Sync + 'static, S: Send + Sync> FromRequestParts<S> for TenantContext<T> {
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<T>()
            .cloned()
            .map(TenantContext)
            .ok_or_else(|| TenantError::MissingTenant.into())
    }
}

/// An extractor that optionally retrieves an identified tenant from request extensions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OptionalTenant<T = Tenant>(pub Option<T>);

impl<T> OptionalTenant<T> {
    pub fn into_inner(self) -> Option<T> {
        self.0
    }
}

impl<T> Deref for OptionalTenant<T> {
    type Target = Option<T>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T: Clone + Send + Sync + 'static, S: Send + Sync> FromRequestParts<S> for OptionalTenant<T> {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        Ok(OptionalTenant(parts.extensions.get::<T>().cloned()))
    }
}
