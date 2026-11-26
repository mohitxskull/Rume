//! Dynamic tenant-scoped RBAC and ABAC authorization.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use axum::body::Body;
use axum::http::Request;
use axum::response::IntoResponse;
use rume_data::persistence::{DatabaseBackend, DatabaseTransaction, DbValue};
use rume_data::sql;
use rume_foundation::error::AppError;
use tokio::sync::RwLock;

use crate::auth::error::AuthError;
use crate::auth::identity::AuthUser;
use crate::auth::session::BoxFuture;
use crate::tenant::model::Tenant;

/// A role definition with optional tenant scope and granted permissions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Role {
    /// Unique role ID.
    pub id: String,
    /// Optional tenant scope (`None` for global roles).
    pub tenant_id: Option<String>,
    /// Role name.
    pub name: String,
    /// Human-readable description.
    pub description: String,
    /// Permissions granted to this role.
    pub permissions: Vec<String>,
}

/// Async storage interface for dynamic roles and user-role assignments.
pub trait RoleStore: Send + Sync + 'static {
    /// Creates a new role.
    fn create_role<'a>(&'a self, role: &'a Role) -> BoxFuture<'a, Result<(), AuthError>>;

    /// Deletes a role by ID.
    fn delete_role<'a>(&'a self, role_id: &'a str) -> BoxFuture<'a, Result<bool, AuthError>>;

    /// Grants `permission` to `role_id`.
    fn grant_permission<'a>(
        &'a self,
        role_id: &'a str,
        permission: &'a str,
    ) -> BoxFuture<'a, Result<(), AuthError>>;

    /// Revokes `permission` from `role_id`.
    fn revoke_permission<'a>(
        &'a self,
        role_id: &'a str,
        permission: &'a str,
    ) -> BoxFuture<'a, Result<(), AuthError>>;

    /// Assigns `role_id` to `user_id` within `tenant_id` (or global if `None`).
    fn assign_role<'a>(
        &'a self,
        user_id: &'a str,
        tenant_id: Option<&'a str>,
        role_id: &'a str,
    ) -> BoxFuture<'a, Result<(), AuthError>>;

    /// Unassigns `role_id` from `user_id` within `tenant_id`.
    fn unassign_role<'a>(
        &'a self,
        user_id: &'a str,
        tenant_id: Option<&'a str>,
        role_id: &'a str,
    ) -> BoxFuture<'a, Result<(), AuthError>>;

    /// Computes effective permissions for `user_id` in `tenant_id` (including global roles).
    fn effective_permissions<'a>(
        &'a self,
        user_id: &'a str,
        tenant_id: Option<&'a str>,
    ) -> BoxFuture<'a, Result<BTreeSet<String>, AuthError>>;

    /// Lists role IDs assigned to `user_id` in `tenant_id`.
    fn user_roles<'a>(
        &'a self,
        user_id: &'a str,
        tenant_id: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Vec<String>, AuthError>>;
}

fn select_sql(
    dialect: &str,
    pg: &'static str,
    mysql: &'static str,
    sqlite: &'static str,
) -> &'static str {
    match dialect {
        "postgres" => pg,
        "mysql" => mysql,
        _ => sqlite,
    }
}

fn to_sql_tenant(tenant_id: Option<&str>) -> Result<&str, AuthError> {
    match tenant_id {
        None => Ok(""),
        Some("") => Err(AuthError::InvalidCredentials(
            "tenant_id cannot be empty".into(),
        )),
        Some(t) => Ok(t),
    }
}

#[derive(Debug, Default)]
struct InMemoryRoleState {
    roles: HashMap<String, Role>,
    role_permissions: HashMap<String, BTreeSet<String>>,
    user_roles: HashMap<(String, String), BTreeSet<String>>,
}

/// In-memory [`RoleStore`] for tests and single-node setups.
#[derive(Debug, Default)]
pub struct InMemoryRoleStore {
    state: RwLock<InMemoryRoleState>,
}

impl InMemoryRoleStore {
    /// Creates an empty in-memory role store.
    pub fn new() -> Self {
        Self::default()
    }
}

impl RoleStore for InMemoryRoleStore {
    fn create_role<'a>(&'a self, role: &'a Role) -> BoxFuture<'a, Result<(), AuthError>> {
        Box::pin(async move {
            to_sql_tenant(role.tenant_id.as_deref())?;
            let mut guard = self.state.write().await;
            if guard.roles.contains_key(&role.id) {
                return Err(AuthError::ProviderError(format!(
                    "role '{}' already exists",
                    role.id
                )));
            }
            guard.roles.insert(role.id.clone(), role.clone());
            guard
                .role_permissions
                .insert(role.id.clone(), role.permissions.iter().cloned().collect());
            Ok(())
        })
    }

    fn delete_role<'a>(&'a self, role_id: &'a str) -> BoxFuture<'a, Result<bool, AuthError>> {
        Box::pin(async move {
            let mut guard = self.state.write().await;
            let existed = guard.roles.remove(role_id).is_some();
            if existed {
                guard.role_permissions.remove(role_id);
                for roles in guard.user_roles.values_mut() {
                    roles.remove(role_id);
                }
            }
            Ok(existed)
        })
    }

    fn grant_permission<'a>(
        &'a self,
        role_id: &'a str,
        permission: &'a str,
    ) -> BoxFuture<'a, Result<(), AuthError>> {
        Box::pin(async move {
            let mut guard = self.state.write().await;
            if !guard.roles.contains_key(role_id) {
                return Err(AuthError::ProviderError(format!(
                    "role '{role_id}' not found"
                )));
            }
            guard
                .role_permissions
                .entry(role_id.to_string())
                .or_default()
                .insert(permission.to_string());
            Ok(())
        })
    }

    fn revoke_permission<'a>(
        &'a self,
        role_id: &'a str,
        permission: &'a str,
    ) -> BoxFuture<'a, Result<(), AuthError>> {
        Box::pin(async move {
            let mut guard = self.state.write().await;
            if let Some(perms) = guard.role_permissions.get_mut(role_id) {
                perms.remove(permission);
            }
            Ok(())
        })
    }

    fn assign_role<'a>(
        &'a self,
        user_id: &'a str,
        tenant_id: Option<&'a str>,
        role_id: &'a str,
    ) -> BoxFuture<'a, Result<(), AuthError>> {
        Box::pin(async move {
            let tenant = to_sql_tenant(tenant_id)?.to_string();
            let mut guard = self.state.write().await;
            if !guard.roles.contains_key(role_id) {
                return Err(AuthError::ProviderError(format!(
                    "role '{role_id}' not found"
                )));
            }
            guard
                .user_roles
                .entry((user_id.to_string(), tenant))
                .or_default()
                .insert(role_id.to_string());
            Ok(())
        })
    }

    fn unassign_role<'a>(
        &'a self,
        user_id: &'a str,
        tenant_id: Option<&'a str>,
        role_id: &'a str,
    ) -> BoxFuture<'a, Result<(), AuthError>> {
        Box::pin(async move {
            let tenant = to_sql_tenant(tenant_id)?.to_string();
            let mut guard = self.state.write().await;
            if let Some(roles) = guard.user_roles.get_mut(&(user_id.to_string(), tenant)) {
                roles.remove(role_id);
            }
            Ok(())
        })
    }

    fn effective_permissions<'a>(
        &'a self,
        user_id: &'a str,
        tenant_id: Option<&'a str>,
    ) -> BoxFuture<'a, Result<BTreeSet<String>, AuthError>> {
        Box::pin(async move {
            let role_ids = self.user_roles(user_id, tenant_id).await?;
            let guard = self.state.read().await;
            let mut perms = BTreeSet::new();
            for role_id in role_ids {
                if let Some(role_perms) = guard.role_permissions.get(&role_id) {
                    perms.extend(role_perms.iter().cloned());
                }
            }
            Ok(perms)
        })
    }

    fn user_roles<'a>(
        &'a self,
        user_id: &'a str,
        tenant_id: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Vec<String>, AuthError>> {
        Box::pin(async move {
            let tenant = to_sql_tenant(tenant_id)?;
            let guard = self.state.read().await;
            let mut seen = BTreeSet::new();
            let mut out = Vec::new();
            for scope in [tenant, ""] {
                if let Some(ids) = guard
                    .user_roles
                    .get(&(user_id.to_string(), scope.to_string()))
                {
                    for id in ids {
                        if seen.insert(id.clone()) {
                            out.push(id.clone());
                        }
                    }
                }
            }
            Ok(out)
        })
    }
}

/// SQL-backed [`RoleStore`] using `rume-data` queries.
#[derive(Debug, Clone)]
pub struct SqlRoleStore<B: DatabaseBackend> {
    backend: Arc<B>,
}

impl<B: DatabaseBackend> SqlRoleStore<B> {
    /// Creates a new [`SqlRoleStore`] backed by `backend`.
    pub fn new(backend: Arc<B>) -> Self {
        Self { backend }
    }
}

impl<B: DatabaseBackend> RoleStore for SqlRoleStore<B> {
    fn create_role<'a>(&'a self, role: &'a Role) -> BoxFuture<'a, Result<(), AuthError>> {
        Box::pin(async move {
            let tenant_sql = to_sql_tenant(role.tenant_id.as_deref())?;
            let dialect = self.backend.dialect();
            let tx = self
                .backend
                .begin_immediate_transaction()
                .await
                .map_err(|e| AuthError::ProviderError(e.to_string()))?;

            let insert_stmt = select_sql(
                dialect,
                sql::PG_AUTH_ROLE_INSERT,
                sql::MYSQL_AUTH_ROLE_INSERT,
                sql::SQLITE_AUTH_ROLE_INSERT,
            );
            tx.execute_params(
                insert_stmt,
                &[
                    DbValue::from(role.id.as_str()),
                    DbValue::from(tenant_sql),
                    DbValue::from(role.name.as_str()),
                    DbValue::from(role.description.as_str()),
                ],
            )
            .await
            .map_err(|e| AuthError::ProviderError(e.to_string()))?;

            let grant_stmt = select_sql(
                dialect,
                sql::PG_AUTH_ROLE_GRANT_PERMISSION,
                sql::MYSQL_AUTH_ROLE_GRANT_PERMISSION,
                sql::SQLITE_AUTH_ROLE_GRANT_PERMISSION,
            );
            for perm in &role.permissions {
                tx.execute_params(
                    grant_stmt,
                    &[
                        DbValue::from(role.id.as_str()),
                        DbValue::from(perm.as_str()),
                    ],
                )
                .await
                .map_err(|e| AuthError::ProviderError(e.to_string()))?;
            }

            tx.commit()
                .await
                .map_err(|e| AuthError::ProviderError(e.to_string()))?;
            Ok(())
        })
    }

    fn delete_role<'a>(&'a self, role_id: &'a str) -> BoxFuture<'a, Result<bool, AuthError>> {
        Box::pin(async move {
            let stmt = select_sql(
                self.backend.dialect(),
                sql::PG_AUTH_ROLE_DELETE,
                sql::MYSQL_AUTH_ROLE_DELETE,
                sql::SQLITE_AUTH_ROLE_DELETE,
            );
            let affected = self
                .backend
                .execute_params(stmt, &[DbValue::from(role_id)])
                .await
                .map_err(|e| AuthError::ProviderError(e.to_string()))?;
            Ok(affected > 0)
        })
    }

    fn grant_permission<'a>(
        &'a self,
        role_id: &'a str,
        permission: &'a str,
    ) -> BoxFuture<'a, Result<(), AuthError>> {
        Box::pin(async move {
            let stmt = select_sql(
                self.backend.dialect(),
                sql::PG_AUTH_ROLE_GRANT_PERMISSION,
                sql::MYSQL_AUTH_ROLE_GRANT_PERMISSION,
                sql::SQLITE_AUTH_ROLE_GRANT_PERMISSION,
            );
            self.backend
                .execute_params(stmt, &[DbValue::from(role_id), DbValue::from(permission)])
                .await
                .map_err(|e| AuthError::ProviderError(e.to_string()))?;
            Ok(())
        })
    }

    fn revoke_permission<'a>(
        &'a self,
        role_id: &'a str,
        permission: &'a str,
    ) -> BoxFuture<'a, Result<(), AuthError>> {
        Box::pin(async move {
            let stmt = select_sql(
                self.backend.dialect(),
                sql::PG_AUTH_ROLE_REVOKE_PERMISSION,
                sql::MYSQL_AUTH_ROLE_REVOKE_PERMISSION,
                sql::SQLITE_AUTH_ROLE_REVOKE_PERMISSION,
            );
            self.backend
                .execute_params(stmt, &[DbValue::from(role_id), DbValue::from(permission)])
                .await
                .map_err(|e| AuthError::ProviderError(e.to_string()))?;
            Ok(())
        })
    }

    fn assign_role<'a>(
        &'a self,
        user_id: &'a str,
        tenant_id: Option<&'a str>,
        role_id: &'a str,
    ) -> BoxFuture<'a, Result<(), AuthError>> {
        Box::pin(async move {
            let tenant_sql = to_sql_tenant(tenant_id)?;
            let stmt = select_sql(
                self.backend.dialect(),
                sql::PG_AUTH_USER_ROLE_ASSIGN,
                sql::MYSQL_AUTH_USER_ROLE_ASSIGN,
                sql::SQLITE_AUTH_USER_ROLE_ASSIGN,
            );
            self.backend
                .execute_params(
                    stmt,
                    &[
                        DbValue::from(user_id),
                        DbValue::from(tenant_sql),
                        DbValue::from(role_id),
                    ],
                )
                .await
                .map_err(|e| AuthError::ProviderError(e.to_string()))?;
            Ok(())
        })
    }

    fn unassign_role<'a>(
        &'a self,
        user_id: &'a str,
        tenant_id: Option<&'a str>,
        role_id: &'a str,
    ) -> BoxFuture<'a, Result<(), AuthError>> {
        Box::pin(async move {
            let tenant_sql = to_sql_tenant(tenant_id)?;
            let stmt = select_sql(
                self.backend.dialect(),
                sql::PG_AUTH_USER_ROLE_UNASSIGN,
                sql::MYSQL_AUTH_USER_ROLE_UNASSIGN,
                sql::SQLITE_AUTH_USER_ROLE_UNASSIGN,
            );
            self.backend
                .execute_params(
                    stmt,
                    &[
                        DbValue::from(user_id),
                        DbValue::from(tenant_sql),
                        DbValue::from(role_id),
                    ],
                )
                .await
                .map_err(|e| AuthError::ProviderError(e.to_string()))?;
            Ok(())
        })
    }

    fn effective_permissions<'a>(
        &'a self,
        user_id: &'a str,
        tenant_id: Option<&'a str>,
    ) -> BoxFuture<'a, Result<BTreeSet<String>, AuthError>> {
        Box::pin(async move {
            let tenant_sql = to_sql_tenant(tenant_id)?;
            let stmt = select_sql(
                self.backend.dialect(),
                sql::PG_AUTH_USER_EFFECTIVE_PERMISSIONS,
                sql::MYSQL_AUTH_USER_EFFECTIVE_PERMISSIONS,
                sql::SQLITE_AUTH_USER_EFFECTIVE_PERMISSIONS,
            );
            let rows = self
                .backend
                .fetch_all(stmt, &[DbValue::from(user_id), DbValue::from(tenant_sql)])
                .await
                .map_err(|e| AuthError::ProviderError(e.to_string()))?;

            let mut perms = BTreeSet::new();
            for row in rows {
                let perm: String = row
                    .get("permission")
                    .map_err(|e| AuthError::ProviderError(e.to_string()))?;
                perms.insert(perm);
            }
            Ok(perms)
        })
    }

    fn user_roles<'a>(
        &'a self,
        user_id: &'a str,
        tenant_id: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Vec<String>, AuthError>> {
        Box::pin(async move {
            let tenant_sql = to_sql_tenant(tenant_id)?;
            let stmt = select_sql(
                self.backend.dialect(),
                sql::PG_AUTH_USER_ROLES_LIST,
                sql::MYSQL_AUTH_USER_ROLES_LIST,
                sql::SQLITE_AUTH_USER_ROLES_LIST,
            );
            let rows = self
                .backend
                .fetch_all(stmt, &[DbValue::from(user_id), DbValue::from(tenant_sql)])
                .await
                .map_err(|e| AuthError::ProviderError(e.to_string()))?;

            let mut seen = BTreeSet::new();
            let mut roles = Vec::new();
            for row in rows {
                let id: String = row
                    .get("id")
                    .map_err(|e| AuthError::ProviderError(e.to_string()))?;
                if seen.insert(id.clone()) {
                    roles.push(id);
                }
            }
            Ok(roles)
        })
    }
}

/// Request/resource attribute context evaluated by [`AbacPolicy`].
#[derive(Debug, Clone)]
pub struct AbacContext {
    /// Authenticated user.
    pub user: AuthUser,
    /// Optional resolved tenant ID.
    pub tenant_id: Option<String>,
    /// Requested action (e.g. `"read"`, `"write"`, `"delete"`).
    pub action: String,
    /// Target resource type (e.g. `"invoice"`).
    pub resource_type: String,
    /// Optional target resource ID.
    pub resource_id: Option<String>,
    /// Arbitrary request/resource attributes.
    pub attributes: BTreeMap<String, String>,
}

/// Async attribute-based access control policy.
pub trait AbacPolicy: Send + Sync + 'static {
    /// Evaluates whether access should be granted for `ctx`.
    fn evaluate<'a>(&'a self, ctx: &'a AbacContext) -> BoxFuture<'a, Result<bool, AuthError>>;
}

/// Combinator that grants access only if **all** inner policies return `Ok(true)`.
#[derive(Default)]
pub struct AllOfPolicy {
    policies: Vec<Arc<dyn AbacPolicy>>,
}

impl std::fmt::Debug for AllOfPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AllOfPolicy")
            .field("count", &self.policies.len())
            .finish()
    }
}

impl AllOfPolicy {
    /// Creates an empty [`AllOfPolicy`].
    pub fn new(policies: Vec<Arc<dyn AbacPolicy>>) -> Self {
        Self { policies }
    }
}

impl AbacPolicy for AllOfPolicy {
    fn evaluate<'a>(&'a self, ctx: &'a AbacContext) -> BoxFuture<'a, Result<bool, AuthError>> {
        Box::pin(async move {
            for policy in &self.policies {
                if !policy.evaluate(ctx).await? {
                    return Ok(false);
                }
            }
            Ok(true)
        })
    }
}

/// Combinator that grants access if **any** inner policy returns `Ok(true)`.
#[derive(Default)]
pub struct AnyOfPolicy {
    policies: Vec<Arc<dyn AbacPolicy>>,
}

impl std::fmt::Debug for AnyOfPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnyOfPolicy")
            .field("count", &self.policies.len())
            .finish()
    }
}

impl AnyOfPolicy {
    /// Creates an empty [`AnyOfPolicy`].
    pub fn new(policies: Vec<Arc<dyn AbacPolicy>>) -> Self {
        Self { policies }
    }
}

impl AbacPolicy for AnyOfPolicy {
    fn evaluate<'a>(&'a self, ctx: &'a AbacContext) -> BoxFuture<'a, Result<bool, AuthError>> {
        Box::pin(async move {
            for policy in &self.policies {
                if policy.evaluate(ctx).await? {
                    return Ok(true);
                }
            }
            Ok(false)
        })
    }
}

/// Dynamic permission check backed by a live [`RoleStore`].
#[derive(Debug, Clone)]
pub struct RequireDynamicPermission<S: RoleStore> {
    store: Arc<S>,
    permission: String,
}

impl<S: RoleStore> RequireDynamicPermission<S> {
    /// Creates a policy requiring `permission` in `store`.
    pub fn new(store: Arc<S>, permission: impl Into<String>) -> Self {
        Self {
            store,
            permission: permission.into(),
        }
    }
}

impl<S: RoleStore> AbacPolicy for RequireDynamicPermission<S> {
    fn evaluate<'a>(&'a self, ctx: &'a AbacContext) -> BoxFuture<'a, Result<bool, AuthError>> {
        Box::pin(async move {
            let resolved_tenant = ctx.tenant_id.as_deref().or(ctx.user.tenant_id.as_deref());
            let perms = self
                .store
                .effective_permissions(&ctx.user.id, resolved_tenant)
                .await?;
            Ok(perms.contains(&self.permission))
        })
    }
}

/// Tower layer enforcing an [`AbacPolicy`] on incoming requests.
#[derive(Clone)]
pub struct DynamicPolicyLayer<P: AbacPolicy> {
    policy: Arc<P>,
    action: String,
    resource_type: String,
}

impl<P: AbacPolicy> std::fmt::Debug for DynamicPolicyLayer<P> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DynamicPolicyLayer")
            .field("action", &self.action)
            .field("resource_type", &self.resource_type)
            .finish()
    }
}

impl<P: AbacPolicy> DynamicPolicyLayer<P> {
    /// Creates a new [`DynamicPolicyLayer`].
    pub fn new(
        policy: Arc<P>,
        action: impl Into<String>,
        resource_type: impl Into<String>,
    ) -> Self {
        Self {
            policy,
            action: action.into(),
            resource_type: resource_type.into(),
        }
    }
}

impl<S, P: AbacPolicy> tower::Layer<S> for DynamicPolicyLayer<P> {
    type Service = DynamicPolicyService<S, P>;

    fn layer(&self, inner: S) -> Self::Service {
        DynamicPolicyService {
            inner,
            policy: Arc::clone(&self.policy),
            action: self.action.clone(),
            resource_type: self.resource_type.clone(),
        }
    }
}

/// Tower service for [`DynamicPolicyLayer`].
#[derive(Clone)]
pub struct DynamicPolicyService<S, P: AbacPolicy> {
    inner: S,
    policy: Arc<P>,
    action: String,
    resource_type: String,
}

impl<S, P> tower::Service<axum::http::Request<axum::body::Body>> for DynamicPolicyService<S, P>
where
    S: tower::Service<
            axum::http::Request<axum::body::Body>,
            Response = axum::http::Response<axum::body::Body>,
        > + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
    P: AbacPolicy,
{
    type Response = axum::http::Response<axum::body::Body>;
    type Error = S::Error;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<Body>) -> Self::Future {
        let policy = Arc::clone(&self.policy);
        let action = self.action.clone();
        let resource_type = self.resource_type.clone();
        let clone = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, clone);

        Box::pin(async move {
            let Some(user) = req.extensions().get::<AuthUser>().cloned() else {
                return Ok(AppError::Unauthorized.into_response());
            };

            let tenant_id = req
                .extensions()
                .get::<Tenant>()
                .map(|t| t.id.clone())
                .or_else(|| user.tenant_id.clone());

            let ctx = AbacContext {
                user,
                tenant_id,
                action,
                resource_type,
                resource_id: None,
                attributes: BTreeMap::new(),
            };

            match policy.evaluate(&ctx).await {
                Ok(true) => inner.call(req).await,
                Ok(false) => Ok(AppError::Forbidden.into_response()),
                Err(err) => Ok(AppError::from(err).into_response()),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, Response, StatusCode};
    use tower::{Layer, ServiceExt};

    use super::*;
    use crate::tenant::model::Tenant;
    use rume_data::persistence::{Migrator, SqliteBackend};

    async fn setup_sqlite() -> Arc<SqliteBackend> {
        let backend = Arc::new(SqliteBackend::in_memory().await.unwrap());
        let migrator = Migrator::new().with_rume_auth("sqlite");
        migrator.run_pending(backend.as_ref()).await.unwrap();
        backend
    }

    struct AttrEqPolicy {
        key: String,
        expected: String,
    }

    impl AbacPolicy for AttrEqPolicy {
        fn evaluate<'a>(&'a self, ctx: &'a AbacContext) -> BoxFuture<'a, Result<bool, AuthError>> {
            Box::pin(async move { Ok(ctx.attributes.get(&self.key) == Some(&self.expected)) })
        }
    }

    #[tokio::test]
    async fn sql_role_store_enforces_tenant_isolation_for_roles_and_permissions() {
        let backend = setup_sqlite().await;
        let store = SqlRoleStore::new(backend);

        store
            .create_role(&Role {
                id: "role-a-admin".into(),
                tenant_id: Some("tenant-a".into()),
                name: "admin".into(),
                description: "Tenant A admin".into(),
                permissions: vec!["billing:read".into(), "billing:write".into()],
            })
            .await
            .unwrap();

        store
            .create_role(&Role {
                id: "role-b-viewer".into(),
                tenant_id: Some("tenant-b".into()),
                name: "viewer".into(),
                description: "Tenant B viewer".into(),
                permissions: vec!["docs:read".into()],
            })
            .await
            .unwrap();

        store
            .assign_role("user-1", Some("tenant-a"), "role-a-admin")
            .await
            .unwrap();
        store
            .assign_role("user-1", Some("tenant-b"), "role-b-viewer")
            .await
            .unwrap();

        let perms_a = store
            .effective_permissions("user-1", Some("tenant-a"))
            .await
            .unwrap();
        assert_eq!(
            perms_a,
            BTreeSet::from(["billing:read".to_string(), "billing:write".to_string()])
        );

        let perms_b = store
            .effective_permissions("user-1", Some("tenant-b"))
            .await
            .unwrap();
        assert_eq!(perms_b, BTreeSet::from(["docs:read".to_string()]));

        // Revoking a permission updates effective_permissions immediately.
        store
            .revoke_permission("role-a-admin", "billing:write")
            .await
            .unwrap();
        let perms_a_after = store
            .effective_permissions("user-1", Some("tenant-a"))
            .await
            .unwrap();
        assert_eq!(perms_a_after, BTreeSet::from(["billing:read".to_string()]));
    }

    #[tokio::test]
    async fn sql_role_store_global_roles_deduplicate_and_query_cleanly_via_empty_sentinel() {
        let backend = setup_sqlite().await;
        let store = SqlRoleStore::new(backend);

        // Empty tenant_id string is rejected so it cannot collide with the global sentinel.
        let invalid_role = Role {
            id: "role-bad".into(),
            tenant_id: Some(String::new()),
            name: "bad".into(),
            description: String::new(),
            permissions: vec![],
        };
        assert!(store.create_role(&invalid_role).await.is_err());

        store
            .create_role(&Role {
                id: "role-global-auditor".into(),
                tenant_id: None,
                name: "auditor".into(),
                description: "Global auditor".into(),
                permissions: vec!["audit:read".into()],
            })
            .await
            .unwrap();

        // Assign globally and also in tenant-a to verify deduplication.
        store
            .assign_role("user-1", None, "role-global-auditor")
            .await
            .unwrap();
        store
            .assign_role("user-1", Some("tenant-a"), "role-global-auditor")
            .await
            .unwrap();

        let roles_in_a = store.user_roles("user-1", Some("tenant-a")).await.unwrap();
        assert_eq!(roles_in_a, vec!["role-global-auditor".to_string()]);

        let perms_in_a = store
            .effective_permissions("user-1", Some("tenant-a"))
            .await
            .unwrap();
        assert_eq!(perms_in_a, BTreeSet::from(["audit:read".to_string()]));

        let global_perms = store.effective_permissions("user-1", None).await.unwrap();
        assert_eq!(global_perms, BTreeSet::from(["audit:read".to_string()]));
    }

    #[tokio::test]
    async fn require_dynamic_permission_and_dynamic_policy_layer_enforce_live_revocation() {
        let backend = setup_sqlite().await;
        let store = Arc::new(SqlRoleStore::new(backend));

        store
            .create_role(&Role {
                id: "role-editor".into(),
                tenant_id: Some("tenant-1".into()),
                name: "editor".into(),
                description: String::new(),
                permissions: vec!["invoice:write".into()],
            })
            .await
            .unwrap();
        store
            .assign_role("user-1", Some("tenant-1"), "role-editor")
            .await
            .unwrap();

        let policy = Arc::new(RequireDynamicPermission::new(
            Arc::clone(&store),
            "invoice:write",
        ));
        let layer = DynamicPolicyLayer::new(policy, "write", "invoice");
        let svc = layer.layer(tower::service_fn(|_req: Request<Body>| async {
            Ok::<_, std::convert::Infallible>(Response::new(Body::from("ok")))
        }));

        // Unauthenticated -> 401.
        let unauth_resp = svc
            .clone()
            .oneshot(Request::builder().body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(unauth_resp.status(), StatusCode::UNAUTHORIZED);

        // Authenticated + Tenant -> 200.
        let mut req_ok = Request::builder().body(Body::empty()).unwrap();
        req_ok.extensions_mut().insert(AuthUser::new("user-1"));
        req_ok
            .extensions_mut()
            .insert(Tenant::new("tenant-1", "t1"));
        let ok_resp = svc.clone().oneshot(req_ok).await.unwrap();
        assert_eq!(ok_resp.status(), StatusCode::OK);

        // Revoke permission live -> 403.
        store
            .revoke_permission("role-editor", "invoice:write")
            .await
            .unwrap();
        let mut req_denied = Request::builder().body(Body::empty()).unwrap();
        req_denied.extensions_mut().insert(AuthUser::new("user-1"));
        req_denied
            .extensions_mut()
            .insert(Tenant::new("tenant-1", "t1"));
        let denied_resp = svc.oneshot(req_denied).await.unwrap();
        assert_eq!(denied_resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn abac_policy_combinators_all_of_and_any_of_evaluate_attributes() {
        let p_env = Arc::new(AttrEqPolicy {
            key: "env".into(),
            expected: "prod".into(),
        });
        let p_region = Arc::new(AttrEqPolicy {
            key: "region".into(),
            expected: "us-east".into(),
        });

        let all = AllOfPolicy::new(vec![p_env.clone(), p_region.clone()]);
        let any = AnyOfPolicy::new(vec![p_env, p_region]);

        let mut attrs = BTreeMap::new();
        attrs.insert("env".into(), "prod".into());
        let ctx_partial = AbacContext {
            user: AuthUser::new("u1"),
            tenant_id: None,
            action: "read".into(),
            resource_type: "secret".into(),
            resource_id: None,
            attributes: attrs.clone(),
        };

        assert!(!all.evaluate(&ctx_partial).await.unwrap());
        assert!(any.evaluate(&ctx_partial).await.unwrap());

        attrs.insert("region".into(), "us-east".into());
        let ctx_full = AbacContext {
            attributes: attrs,
            ..ctx_partial
        };
        assert!(all.evaluate(&ctx_full).await.unwrap());
    }
}
