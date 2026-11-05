//! Declarative authorization policies and role/permission checks.

use std::fmt::Debug;

use crate::auth::identity::AuthUser;
use crate::error::AppError;

/// An authorization policy evaluating whether an authenticated subject is allowed to perform an action on a resource.
pub trait Policy<T = AuthUser, R = ()>: Send + Sync + Debug {
    /// Evaluates if the identity is authorized to access the given resource.
    fn is_authorized(&self, identity: &T, resource: &R) -> bool;
}

/// Enforces that an identity is authorized under the specified policy, returning [`AppError::Forbidden`] on denial.
pub fn authorize<T, R, P>(identity: &T, resource: &R, policy: P) -> Result<(), AppError>
where
    P: Policy<T, R>,
{
    if policy.is_authorized(identity, resource) {
        Ok(())
    } else {
        Err(AppError::Forbidden)
    }
}

/// Policy requiring the identity to possess a specific role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequireRole(pub &'static str);

impl<R> Policy<AuthUser, R> for RequireRole {
    fn is_authorized(&self, identity: &AuthUser, _resource: &R) -> bool {
        identity.has_role(self.0)
    }
}

/// Policy requiring the identity to possess any of the specified roles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequireAnyRole(pub &'static [&'static str]);

impl<R> Policy<AuthUser, R> for RequireAnyRole {
    fn is_authorized(&self, identity: &AuthUser, _resource: &R) -> bool {
        identity.has_any_role(self.0)
    }
}

/// Policy requiring the identity to possess a specific permission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequirePermission(pub &'static str);

impl<R> Policy<AuthUser, R> for RequirePermission {
    fn is_authorized(&self, identity: &AuthUser, _resource: &R) -> bool {
        identity.has_permission(self.0)
    }
}

/// Policy requiring the identity to possess all specified permissions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequireAllPermissions(pub &'static [&'static str]);

impl<R> Policy<AuthUser, R> for RequireAllPermissions {
    fn is_authorized(&self, identity: &AuthUser, _resource: &R) -> bool {
        identity.has_all_permissions(self.0)
    }
}

/// Custom closure-based policy.
pub struct CustomPolicy<F, R = ()> {
    func: F,
    _phantom: std::marker::PhantomData<fn(&R)>,
}

impl<F, R> std::fmt::Debug for CustomPolicy<F, R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CustomPolicy").finish()
    }
}

impl<F, R> CustomPolicy<F, R>
where
    F: Fn(&AuthUser, &R) -> bool + Send + Sync,
    R: Send + Sync,
{
    /// Creates a custom policy from a closure.
    pub fn new(func: F) -> Self {
        Self {
            func,
            _phantom: std::marker::PhantomData,
        }
    }
}

impl<F, R> Policy<AuthUser, R> for CustomPolicy<F, R>
where
    F: Fn(&AuthUser, &R) -> bool + Send + Sync,
    R: Send + Sync,
{
    fn is_authorized(&self, identity: &AuthUser, resource: &R) -> bool {
        (self.func)(identity, resource)
    }
}
