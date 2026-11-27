//! SQL-backed [`SessionStore`] implementation.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use rume_data::persistence::{DatabaseBackend, DatabaseTransaction, DbRow, DbValue};
use rume_data::sql;

use crate::auth::error::AuthError;
use crate::auth::session::{BoxFuture, Session, SessionStore};

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

fn current_epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn decode_session_row(row: &DbRow) -> Result<Session, AuthError> {
    let map_err =
        |e: rume_data::persistence::PersistenceError| AuthError::ProviderError(e.to_string());
    let roles_json: String = row.get("roles_json").map_err(map_err)?;
    let permissions_json: String = row.get("permissions_json").map_err(map_err)?;
    let data_json: String = row.get("data_json").map_err(map_err)?;

    let roles: Vec<String> = serde_json::from_str(&roles_json)
        .map_err(|e| AuthError::ProviderError(format!("invalid roles_json: {e}")))?;
    let permissions: Vec<String> = serde_json::from_str(&permissions_json)
        .map_err(|e| AuthError::ProviderError(format!("invalid permissions_json: {e}")))?;
    let data: BTreeMap<String, String> = serde_json::from_str(&data_json)
        .map_err(|e| AuthError::ProviderError(format!("invalid data_json: {e}")))?;

    Ok(Session {
        id: row.get("id").map_err(map_err)?,
        user_id: row.get("user_id").map_err(map_err)?,
        tenant_id: row.get_opt("tenant_id").map_err(map_err)?,
        roles,
        permissions,
        expires_at: row.get("expires_at").map_err(map_err)?,
        data,
    })
}

fn upsert_params(session: &Session, created_at: u64) -> Result<[DbValue; 8], AuthError> {
    let roles_json = serde_json::to_string(&session.roles)
        .map_err(|e| AuthError::ProviderError(e.to_string()))?;
    let permissions_json = serde_json::to_string(&session.permissions)
        .map_err(|e| AuthError::ProviderError(e.to_string()))?;
    let data_json = serde_json::to_string(&session.data)
        .map_err(|e| AuthError::ProviderError(e.to_string()))?;
    Ok([
        DbValue::from(session.id.as_str()),
        DbValue::from(session.user_id.as_str()),
        DbValue::from(session.tenant_id.as_deref()),
        DbValue::from(roles_json),
        DbValue::from(permissions_json),
        DbValue::from(data_json),
        DbValue::from(session.expires_at),
        DbValue::from(created_at),
    ])
}

/// Persistent SQL [`SessionStore`] across SQLite, PostgreSQL, and MySQL.
#[derive(Debug, Clone)]
pub struct SqlSessionStore<B: DatabaseBackend> {
    backend: Arc<B>,
}

impl<B: DatabaseBackend> SqlSessionStore<B> {
    /// Creates a new [`SqlSessionStore`] backed by `backend`.
    pub fn new(backend: Arc<B>) -> Self {
        Self { backend }
    }

    /// Deletes all expired sessions as of `now_epoch_sec`, returning the number removed.
    pub async fn prune_expired(&self, now_epoch_sec: u64) -> Result<u64, AuthError> {
        let stmt = select_sql(
            self.backend.dialect(),
            sql::PG_AUTH_SESSION_PRUNE_EXPIRED,
            sql::MYSQL_AUTH_SESSION_PRUNE_EXPIRED,
            sql::SQLITE_AUTH_SESSION_PRUNE_EXPIRED,
        );
        self.backend
            .execute_params(stmt, &[DbValue::from(now_epoch_sec)])
            .await
            .map_err(|e| AuthError::ProviderError(e.to_string()))
    }
}

impl<B: DatabaseBackend> SessionStore for SqlSessionStore<B> {
    fn load(&self, id: &str) -> BoxFuture<'_, Result<Option<Session>, AuthError>> {
        let id = id.to_owned();
        Box::pin(async move {
            let stmt = select_sql(
                self.backend.dialect(),
                sql::PG_AUTH_SESSION_LOAD,
                sql::MYSQL_AUTH_SESSION_LOAD,
                sql::SQLITE_AUTH_SESSION_LOAD,
            );
            let Some(row) = self
                .backend
                .fetch_optional(stmt, &[DbValue::from(id)])
                .await
                .map_err(|e| AuthError::ProviderError(e.to_string()))?
            else {
                return Ok(None);
            };
            let session = decode_session_row(&row)?;
            if session.is_expired() {
                return Ok(None);
            }
            Ok(Some(session))
        })
    }

    fn save(&self, session: Session) -> BoxFuture<'_, Result<(), AuthError>> {
        Box::pin(async move {
            let params = upsert_params(&session, current_epoch_secs())?;
            let stmt = select_sql(
                self.backend.dialect(),
                sql::PG_AUTH_SESSION_UPSERT,
                sql::MYSQL_AUTH_SESSION_UPSERT,
                sql::SQLITE_AUTH_SESSION_UPSERT,
            );
            self.backend
                .execute_params(stmt, &params)
                .await
                .map_err(|e| AuthError::ProviderError(e.to_string()))?;
            Ok(())
        })
    }

    fn destroy(&self, id: &str) -> BoxFuture<'_, Result<(), AuthError>> {
        let id = id.to_owned();
        Box::pin(async move {
            let stmt = select_sql(
                self.backend.dialect(),
                sql::PG_AUTH_SESSION_DELETE,
                sql::MYSQL_AUTH_SESSION_DELETE,
                sql::SQLITE_AUTH_SESSION_DELETE,
            );
            self.backend
                .execute_params(stmt, &[DbValue::from(id)])
                .await
                .map_err(|e| AuthError::ProviderError(e.to_string()))?;
            Ok(())
        })
    }

    fn regenerate_id(&self, old_id: &str) -> BoxFuture<'_, Result<Session, AuthError>> {
        let old_id = old_id.to_owned();
        Box::pin(async move {
            let dialect = self.backend.dialect();
            let tx = self
                .backend
                .begin_immediate_transaction()
                .await
                .map_err(|e| AuthError::ProviderError(e.to_string()))?;

            let load_stmt = select_sql(
                dialect,
                sql::PG_AUTH_SESSION_LOAD,
                sql::MYSQL_AUTH_SESSION_LOAD,
                sql::SQLITE_AUTH_SESSION_LOAD,
            );
            let Some(row) = tx
                .fetch_optional(load_stmt, &[DbValue::from(old_id.as_str())])
                .await
                .map_err(|e| AuthError::ProviderError(e.to_string()))?
            else {
                let _ = tx.rollback().await;
                return Err(AuthError::InvalidCredentials("session not found".into()));
            };

            let old_session = decode_session_row(&row)?;
            if old_session.is_expired() {
                let _ = tx.rollback().await;
                return Err(AuthError::InvalidCredentials("session expired".into()));
            }

            let delete_stmt = select_sql(
                dialect,
                sql::PG_AUTH_SESSION_DELETE,
                sql::MYSQL_AUTH_SESSION_DELETE,
                sql::SQLITE_AUTH_SESSION_DELETE,
            );
            let affected = tx
                .execute_params(delete_stmt, &[DbValue::from(old_id.as_str())])
                .await
                .map_err(|e| AuthError::ProviderError(e.to_string()))?;
            if affected != 1 {
                let _ = tx.rollback().await;
                return Err(AuthError::InvalidCredentials(
                    "session concurrently invalidated".into(),
                ));
            }

            let new_session = Session {
                id: uuid::Uuid::new_v4().to_string(),
                ..old_session
            };
            let params = upsert_params(&new_session, current_epoch_secs())?;
            let upsert_stmt = select_sql(
                dialect,
                sql::PG_AUTH_SESSION_UPSERT,
                sql::MYSQL_AUTH_SESSION_UPSERT,
                sql::SQLITE_AUTH_SESSION_UPSERT,
            );
            tx.execute_params(upsert_stmt, &params)
                .await
                .map_err(|e| AuthError::ProviderError(e.to_string()))?;

            tx.commit()
                .await
                .map_err(|e| AuthError::ProviderError(e.to_string()))?;
            Ok(new_session)
        })
    }
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;
    use rume_data::persistence::{Migrator, SqliteBackend};

    fn now_secs() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    async fn setup_sqlite() -> Arc<SqliteBackend> {
        let backend = Arc::new(SqliteBackend::in_memory().await.unwrap());
        let migrator = Migrator::new().with_rume_auth("sqlite");
        migrator.run_pending(backend.as_ref()).await.unwrap();
        backend
    }

    #[tokio::test]
    async fn sql_session_store_save_load_destroy_and_prune_expired_on_sqlite() {
        let backend = setup_sqlite().await;
        let store = SqlSessionStore::new(backend);
        let now = now_secs();

        let active = Session::new("sess-active", "user-1", 600)
            .with_tenant_id("tenant-1")
            .with_role("admin")
            .with_permission("users:write")
            .with_data("theme", "dark");
        store.save(active.clone()).await.unwrap();

        let mut expired = Session::new("sess-expired", "user-2", 0);
        expired.expires_at = now.saturating_sub(10);
        store.save(expired).await.unwrap();

        let loaded = store.load("sess-active").await.unwrap().unwrap();
        assert_eq!(loaded, active);

        // Expired session returns None on load.
        assert!(store.load("sess-expired").await.unwrap().is_none());

        // Prune removes expired session and leaves active one.
        let pruned = store.prune_expired(now).await.unwrap();
        assert_eq!(pruned, 1);
        assert!(store.load("sess-active").await.unwrap().is_some());

        // Destroy removes active session.
        store.destroy("sess-active").await.unwrap();
        assert!(store.load("sess-active").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn sql_session_store_regenerate_id_rotates_atomically_and_rejects_expired_or_missing() {
        let backend = setup_sqlite().await;
        let store = SqlSessionStore::new(backend);
        let now = now_secs();

        // Missing session fails.
        assert!(matches!(
            store.regenerate_id("missing-id").await,
            Err(AuthError::InvalidCredentials(_))
        ));

        // Expired session fails.
        let mut expired = Session::new("sess-exp", "user-exp", 0);
        expired.expires_at = now.saturating_sub(5);
        store.save(expired).await.unwrap();
        assert!(matches!(
            store.regenerate_id("sess-exp").await,
            Err(AuthError::InvalidCredentials(_))
        ));

        // Active session rotates to a new ID, preserves payload, and invalidates old ID.
        let session = Session::new("sess-old", "user-42", 600)
            .with_tenant_id("tenant-a")
            .with_role("editor")
            .with_permission("docs:edit")
            .with_data("k", "v");
        store.save(session).await.unwrap();

        let rotated = store.regenerate_id("sess-old").await.unwrap();
        assert_ne!(rotated.id, "sess-old");
        assert_eq!(rotated.user_id, "user-42");
        assert_eq!(rotated.tenant_id.as_deref(), Some("tenant-a"));
        assert_eq!(rotated.roles, vec!["editor".to_string()]);
        assert_eq!(rotated.permissions, vec!["docs:edit".to_string()]);
        assert_eq!(rotated.data.get("k").map(String::as_str), Some("v"));

        assert!(store.load("sess-old").await.unwrap().is_none());
        assert_eq!(store.load(&rotated.id).await.unwrap().unwrap(), rotated);
    }
}
