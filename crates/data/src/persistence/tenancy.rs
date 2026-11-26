//! Multi-tenant database isolation strategies and pool routing.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, RwLock};

use crate::persistence::backend::{
    BoxFuture, DatabaseBackend, DatabaseTransaction, validate_sql_identifier, validate_tenant_id,
};
use crate::persistence::error::PersistenceError;
use crate::persistence::pool::DatabasePool;

/// Multi-tenant database isolation strategy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TenantIsolationStrategy {
    /// Shared pool with PostgreSQL Row-Level Security (`app.current_tenant`).
    RowLevelSecurity,
    /// Shared pool with per-tenant PostgreSQL schema (`search_path`).
    SchemaPerTenant {
        /// Prefix prepended to the tenant ID to form the schema identifier.
        schema_prefix: String,
    },
    /// Dedicated database pool per tenant.
    DatabasePerTenant,
}

/// Manages database pools and tenant-isolated transactions across isolation strategies.
#[derive(Debug, Clone)]
pub struct TenantPoolManager<B: DatabaseBackend> {
    strategy: TenantIsolationStrategy,
    shared_pool: Option<DatabasePool<B>>,
    tenant_pools: Arc<RwLock<HashMap<String, DatabasePool<B>>>>,
}

impl<B: DatabaseBackend> TenantPoolManager<B> {
    /// Creates a manager using PostgreSQL Row-Level Security on a shared pool.
    pub fn new_rls(pool: DatabasePool<B>) -> Self {
        Self {
            strategy: TenantIsolationStrategy::RowLevelSecurity,
            shared_pool: Some(pool),
            tenant_pools: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Creates a manager using schema-per-tenant isolation on a shared pool.
    pub fn new_schema_per_tenant(pool: DatabasePool<B>, schema_prefix: impl Into<String>) -> Self {
        Self {
            strategy: TenantIsolationStrategy::SchemaPerTenant {
                schema_prefix: schema_prefix.into(),
            },
            shared_pool: Some(pool),
            tenant_pools: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Creates a manager using dedicated per-tenant database pools.
    pub fn new_database_per_tenant() -> Self {
        Self {
            strategy: TenantIsolationStrategy::DatabasePerTenant,
            shared_pool: None,
            tenant_pools: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Returns the configured isolation strategy.
    pub fn strategy(&self) -> &TenantIsolationStrategy {
        &self.strategy
    }

    /// Registers a dedicated pool for a tenant (for [`TenantIsolationStrategy::DatabasePerTenant`]).
    pub fn register_tenant_pool(
        &self,
        tenant_id: impl Into<String>,
        pool: DatabasePool<B>,
    ) -> Result<(), PersistenceError> {
        if !matches!(self.strategy, TenantIsolationStrategy::DatabasePerTenant) {
            return Err(PersistenceError::TransactionError(
                "register_tenant_pool is only supported for DatabasePerTenant strategy".into(),
            ));
        }
        let tenant_id = tenant_id.into();
        validate_tenant_id(&tenant_id)?;
        let mut pools = self.tenant_pools.write().map_err(|e| {
            PersistenceError::TransactionError(format!("tenant pool lock poisoned: {e}"))
        })?;
        pools.insert(tenant_id, pool);
        Ok(())
    }

    /// Removes a dedicated pool for a tenant.
    pub fn remove_tenant_pool(
        &self,
        tenant_id: &str,
    ) -> Result<Option<DatabasePool<B>>, PersistenceError> {
        if !matches!(self.strategy, TenantIsolationStrategy::DatabasePerTenant) {
            return Err(PersistenceError::TransactionError(
                "remove_tenant_pool is only supported for DatabasePerTenant strategy".into(),
            ));
        }
        let mut pools = self.tenant_pools.write().map_err(|e| {
            PersistenceError::TransactionError(format!("tenant pool lock poisoned: {e}"))
        })?;
        Ok(pools.remove(tenant_id))
    }

    /// Returns the pool associated with the given tenant.
    pub fn pool_for_tenant(&self, tenant_id: &str) -> Result<DatabasePool<B>, PersistenceError> {
        validate_tenant_id(tenant_id)?;
        match &self.strategy {
            TenantIsolationStrategy::RowLevelSecurity
            | TenantIsolationStrategy::SchemaPerTenant { .. } => {
                self.shared_pool.clone().ok_or(PersistenceError::Closed)
            }
            TenantIsolationStrategy::DatabasePerTenant => {
                let pools = self.tenant_pools.read().map_err(|e| {
                    PersistenceError::TransactionError(format!("tenant pool lock poisoned: {e}"))
                })?;
                pools.get(tenant_id).cloned().ok_or_else(|| {
                    PersistenceError::ConnectionFailed(format!(
                        "no database pool registered for tenant '{tenant_id}'"
                    ))
                })
            }
        }
    }

    /// Executes `f` inside a tenant-isolated transaction.
    pub fn tenant_transaction<'a, F, Fut, R>(
        &'a self,
        tenant_id: &'a str,
        f: F,
    ) -> BoxFuture<'a, Result<R, PersistenceError>>
    where
        F: FnOnce(B::Transaction) -> Fut + Send + 'a,
        Fut: Future<Output = Result<R, PersistenceError>> + Send + 'a,
        R: Send + 'a,
    {
        Box::pin(async move {
            validate_tenant_id(tenant_id)?;
            match &self.strategy {
                TenantIsolationStrategy::RowLevelSecurity => {
                    let pool = self.pool_for_tenant(tenant_id)?;
                    pool.transaction(|tx| async move {
                        tx.set_tenant_context(tenant_id).await?;
                        f(tx).await
                    })
                    .await
                }
                TenantIsolationStrategy::SchemaPerTenant { schema_prefix } => {
                    let schema = format!("{schema_prefix}{tenant_id}");
                    validate_sql_identifier(&schema)?;
                    let pool = self.pool_for_tenant(tenant_id)?;
                    pool.transaction(|tx| async move {
                        tx.set_tenant_search_path(&schema).await?;
                        f(tx).await
                    })
                    .await
                }
                TenantIsolationStrategy::DatabasePerTenant => {
                    let pool = self.pool_for_tenant(tenant_id)?;
                    pool.transaction(f).await
                }
            }
        })
    }
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use super::*;
    use crate::persistence::backend::{DatabaseTransaction, DbValue};
    use crate::persistence::config::DatabaseConfig;
    use crate::persistence::sqlite::SqliteBackend;

    #[tokio::test]
    async fn tenant_pool_manager_database_per_tenant_routes_and_isolates_sqlite_pools() {
        let backend_a = SqliteBackend::in_memory().await.unwrap();
        backend_a
            .execute("CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT NOT NULL)")
            .await
            .unwrap();
        let pool_a = DatabasePool::new(backend_a, DatabaseConfig::new("sqlite::memory:"));

        let backend_b = SqliteBackend::in_memory().await.unwrap();
        backend_b
            .execute("CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT NOT NULL)")
            .await
            .unwrap();
        let pool_b = DatabasePool::new(backend_b, DatabaseConfig::new("sqlite::memory:"));

        let manager = TenantPoolManager::new_database_per_tenant();
        manager.register_tenant_pool("tenant-a", pool_a).unwrap();
        manager.register_tenant_pool("tenant-b", pool_b).unwrap();

        manager
            .tenant_transaction("tenant-a", |tx| async move {
                tx.execute_params(
                    "INSERT INTO notes (body) VALUES (?)",
                    &[DbValue::from("alpha-secret")],
                )
                .await?;
                Ok(())
            })
            .await
            .unwrap();

        let count_b = manager
            .pool_for_tenant("tenant-b")
            .unwrap()
            .query_scalar("SELECT CAST(COUNT(*) AS TEXT) FROM notes")
            .await
            .unwrap();
        assert_eq!(count_b.as_deref(), Some("0"));

        let count_a = manager
            .pool_for_tenant("tenant-a")
            .unwrap()
            .query_scalar("SELECT CAST(COUNT(*) AS TEXT) FROM notes")
            .await
            .unwrap();
        assert_eq!(count_a.as_deref(), Some("1"));

        let removed = manager.remove_tenant_pool("tenant-b").unwrap();
        assert!(removed.is_some());
        assert!(manager.pool_for_tenant("tenant-b").is_err());
    }

    #[tokio::test]
    async fn tenant_pool_manager_schema_per_tenant_rejects_invalid_schema_identifier() {
        let backend = SqliteBackend::in_memory().await.unwrap();
        let pool = DatabasePool::new(backend, DatabaseConfig::new("sqlite::memory:"));
        let manager = TenantPoolManager::new_schema_per_tenant(pool, "tenant_");

        let err = manager
            .tenant_transaction("bad-hyphen", |_tx| async move { Ok(()) })
            .await
            .unwrap_err();
        assert!(matches!(err, PersistenceError::QueryFailed(_)));
    }
}
