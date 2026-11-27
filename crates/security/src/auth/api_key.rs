//! Persistent API key management (`ApiKeyStore`, `SqlApiKeyStore`, `InMemoryApiKeyStore`).

use std::collections::HashMap;
use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::distr::Alphanumeric;
use rand::{Rng as _, RngExt as _};
use rume_data::persistence::{DatabaseBackend, DatabaseTransaction, DbRow, DbValue};
use rume_data::sql;
use secrecy::{ExposeSecret, SecretString};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tokio::sync::RwLock;

use crate::auth::error::AuthError;
use crate::auth::session::BoxFuture;

/// Persisted metadata for an API key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiKeyRecord {
    /// Unique API key ID.
    pub id: String,
    /// Public lookup prefix (e.g. `rk_live_abcd`).
    pub prefix: String,
    /// SHA-256 hash of the full API key.
    pub key_hash: [u8; 32],
    /// Owning user ID.
    pub user_id: String,
    /// Optional tenant scope.
    pub tenant_id: Option<String>,
    /// Human-readable key name.
    pub name: String,
    /// Granted permission scopes.
    pub scopes: Vec<String>,
    /// Optional expiration timestamp (UNIX epoch seconds).
    pub expires_at: Option<u64>,
    /// Optional revocation timestamp (UNIX epoch seconds).
    pub revoked_at: Option<u64>,
    /// Creation timestamp (UNIX epoch seconds).
    pub created_at: u64,
    /// Optional last-used timestamp (UNIX epoch seconds).
    pub last_used_at: Option<u64>,
}

/// Newly issued or rotated API key containing the one-time plaintext secret.
pub struct IssuedApiKey {
    /// Persisted record metadata.
    pub record: ApiKeyRecord,
    /// Plaintext API key (returned once at creation/rotation).
    pub plaintext: SecretString,
}

impl std::fmt::Debug for IssuedApiKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IssuedApiKey")
            .field("record", &self.record)
            .field("plaintext", &"[REDACTED]")
            .finish()
    }
}

/// Async storage interface for API keys.
pub trait ApiKeyStore: Send + Sync + 'static {
    /// Creates and stores a new API key for `user_id`.
    fn create<'a>(
        &'a self,
        user_id: &'a str,
        tenant_id: Option<&'a str>,
        name: &'a str,
        scopes: Vec<String>,
        expires_at: Option<u64>,
        now_epoch_sec: u64,
    ) -> BoxFuture<'a, Result<IssuedApiKey, AuthError>>;

    /// Finds candidate API keys matching `prefix`.
    fn find_by_prefix<'a>(
        &'a self,
        prefix: &'a str,
    ) -> BoxFuture<'a, Result<Vec<ApiKeyRecord>, AuthError>>;

    /// Verifies a raw API key in constant time, checking revocation and expiry.
    fn verify_key<'a>(
        &'a self,
        raw_key: &'a str,
        now_epoch_sec: u64,
    ) -> BoxFuture<'a, Result<Option<ApiKeyRecord>, AuthError>>;

    /// Revokes an API key by ID.
    fn revoke<'a>(
        &'a self,
        id: &'a str,
        now_epoch_sec: u64,
    ) -> BoxFuture<'a, Result<bool, AuthError>>;

    /// Atomically issues a replacement key and revokes `old_id`.
    fn rotate<'a>(
        &'a self,
        old_id: &'a str,
        new_expires_at: Option<u64>,
        now_epoch_sec: u64,
    ) -> BoxFuture<'a, Result<IssuedApiKey, AuthError>>;

    /// Lists all API keys belonging to `user_id`.
    fn list_for_user<'a>(
        &'a self,
        user_id: &'a str,
    ) -> BoxFuture<'a, Result<Vec<ApiKeyRecord>, AuthError>>;

    /// Updates `last_used_at` for `id`.
    fn touch_last_used<'a>(
        &'a self,
        id: &'a str,
        now_epoch_sec: u64,
    ) -> BoxFuture<'a, Result<(), AuthError>>;
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

fn generate_api_key(
    user_id: &str,
    tenant_id: Option<&str>,
    name: &str,
    scopes: Vec<String>,
    expires_at: Option<u64>,
    now_epoch_sec: u64,
) -> IssuedApiKey {
    let mut rng = rand::rng();
    let prefix_rand: String = (0..12)
        .map(|_| char::from(rng.sample(Alphanumeric)))
        .collect();
    let mut secret_bytes = [0u8; 32];
    rng.fill_bytes(&mut secret_bytes);
    let secret_b64 = URL_SAFE_NO_PAD.encode(secret_bytes);
    let plaintext = format!("rk_{prefix_rand}_{secret_b64}");
    let prefix = plaintext[..12].to_string();
    let key_hash: [u8; 32] = Sha256::digest(plaintext.as_bytes()).into();

    IssuedApiKey {
        record: ApiKeyRecord {
            id: uuid::Uuid::new_v4().to_string(),
            prefix,
            key_hash,
            user_id: user_id.to_string(),
            tenant_id: tenant_id.map(str::to_string),
            name: name.to_string(),
            scopes,
            expires_at,
            revoked_at: None,
            created_at: now_epoch_sec,
            last_used_at: None,
        },
        plaintext: SecretString::from(plaintext),
    }
}

fn verify_against_candidates(
    candidates: Vec<ApiKeyRecord>,
    raw_key: &str,
    now_epoch_sec: u64,
) -> Option<ApiKeyRecord> {
    let incoming_hash: [u8; 32] = Sha256::digest(raw_key.as_bytes()).into();
    let mut matched: Option<ApiKeyRecord> = None;
    for candidate in candidates {
        if bool::from(candidate.key_hash.ct_eq(&incoming_hash)) {
            matched = Some(candidate);
        }
    }
    let record = matched?;
    if record.revoked_at.is_some() {
        return None;
    }
    if record.expires_at.is_some_and(|exp| exp <= now_epoch_sec) {
        return None;
    }
    Some(record)
}

fn extract_lookup_prefix(raw_key: &str) -> Option<&str> {
    if raw_key.len() >= 12 && raw_key.starts_with("rk_") && raw_key.is_ascii() {
        Some(&raw_key[..12])
    } else {
        None
    }
}

fn decode_api_key_row(row: &DbRow) -> Result<ApiKeyRecord, AuthError> {
    let map_err =
        |e: rume_data::persistence::PersistenceError| AuthError::ProviderError(e.to_string());
    let key_hash_hex: String = row.get("key_hash").map_err(map_err)?;
    let key_hash_bytes = hex::decode(&key_hash_hex)
        .map_err(|e| AuthError::ProviderError(format!("invalid api key hash hex: {e}")))?;
    let key_hash: [u8; 32] = key_hash_bytes.try_into().map_err(|_| {
        AuthError::ProviderError("invalid api key hash length, expected 32 bytes".into())
    })?;

    let scopes_json: String = row.get("scopes_json").map_err(map_err)?;
    let scopes: Vec<String> = serde_json::from_str(&scopes_json)
        .map_err(|e| AuthError::ProviderError(format!("invalid scopes_json: {e}")))?;

    Ok(ApiKeyRecord {
        id: row.get("id").map_err(map_err)?,
        prefix: row.get("prefix").map_err(map_err)?,
        key_hash,
        user_id: row.get("user_id").map_err(map_err)?,
        tenant_id: row.get_opt("tenant_id").map_err(map_err)?,
        name: row.get("name").map_err(map_err)?,
        scopes,
        expires_at: row.get_opt("expires_at").map_err(map_err)?,
        revoked_at: row.get_opt("revoked_at").map_err(map_err)?,
        created_at: row.get("created_at").map_err(map_err)?,
        last_used_at: row.get_opt("last_used_at").map_err(map_err)?,
    })
}

fn insert_params(record: &ApiKeyRecord) -> Result<[DbValue; 11], AuthError> {
    let scopes_json = serde_json::to_string(&record.scopes)
        .map_err(|e| AuthError::ProviderError(e.to_string()))?;
    Ok([
        DbValue::from(record.id.as_str()),
        DbValue::from(record.prefix.as_str()),
        DbValue::from(hex::encode(record.key_hash)),
        DbValue::from(record.user_id.as_str()),
        DbValue::from(record.tenant_id.as_deref()),
        DbValue::from(record.name.as_str()),
        DbValue::from(scopes_json),
        DbValue::from(record.expires_at),
        DbValue::from(record.revoked_at),
        DbValue::from(record.created_at),
        DbValue::from(record.last_used_at),
    ])
}

/// In-memory [`ApiKeyStore`] for tests and single-node setups.
#[derive(Debug, Default)]
pub struct InMemoryApiKeyStore {
    records: RwLock<HashMap<String, ApiKeyRecord>>,
}

impl InMemoryApiKeyStore {
    /// Creates an empty in-memory API key store.
    pub fn new() -> Self {
        Self::default()
    }
}

impl ApiKeyStore for InMemoryApiKeyStore {
    fn create<'a>(
        &'a self,
        user_id: &'a str,
        tenant_id: Option<&'a str>,
        name: &'a str,
        scopes: Vec<String>,
        expires_at: Option<u64>,
        now_epoch_sec: u64,
    ) -> BoxFuture<'a, Result<IssuedApiKey, AuthError>> {
        Box::pin(async move {
            let issued =
                generate_api_key(user_id, tenant_id, name, scopes, expires_at, now_epoch_sec);
            self.records
                .write()
                .await
                .insert(issued.record.id.clone(), issued.record.clone());
            Ok(issued)
        })
    }

    fn find_by_prefix<'a>(
        &'a self,
        prefix: &'a str,
    ) -> BoxFuture<'a, Result<Vec<ApiKeyRecord>, AuthError>> {
        Box::pin(async move {
            let guard = self.records.read().await;
            Ok(guard
                .values()
                .filter(|r| r.prefix == prefix)
                .cloned()
                .collect())
        })
    }

    fn verify_key<'a>(
        &'a self,
        raw_key: &'a str,
        now_epoch_sec: u64,
    ) -> BoxFuture<'a, Result<Option<ApiKeyRecord>, AuthError>> {
        Box::pin(async move {
            let Some(prefix) = extract_lookup_prefix(raw_key) else {
                return Ok(None);
            };
            let candidates = self.find_by_prefix(prefix).await?;
            Ok(verify_against_candidates(
                candidates,
                raw_key,
                now_epoch_sec,
            ))
        })
    }

    fn revoke<'a>(
        &'a self,
        id: &'a str,
        now_epoch_sec: u64,
    ) -> BoxFuture<'a, Result<bool, AuthError>> {
        Box::pin(async move {
            let mut guard = self.records.write().await;
            let Some(record) = guard.get_mut(id) else {
                return Ok(false);
            };
            if record.revoked_at.is_some() {
                return Ok(false);
            }
            record.revoked_at = Some(now_epoch_sec);
            Ok(true)
        })
    }

    fn rotate<'a>(
        &'a self,
        old_id: &'a str,
        new_expires_at: Option<u64>,
        now_epoch_sec: u64,
    ) -> BoxFuture<'a, Result<IssuedApiKey, AuthError>> {
        Box::pin(async move {
            let mut guard = self.records.write().await;
            let old = guard
                .get_mut(old_id)
                .ok_or_else(|| AuthError::InvalidCredentials("api key not found".into()))?;
            if old.revoked_at.is_some() {
                return Err(AuthError::InvalidCredentials(
                    "api key already revoked".into(),
                ));
            }
            if old.expires_at.is_some_and(|exp| exp <= now_epoch_sec) {
                return Err(AuthError::Expired);
            }
            old.revoked_at = Some(now_epoch_sec);
            let issued = generate_api_key(
                &old.user_id,
                old.tenant_id.as_deref(),
                &old.name,
                old.scopes.clone(),
                new_expires_at,
                now_epoch_sec,
            );
            guard.insert(issued.record.id.clone(), issued.record.clone());
            Ok(issued)
        })
    }

    fn list_for_user<'a>(
        &'a self,
        user_id: &'a str,
    ) -> BoxFuture<'a, Result<Vec<ApiKeyRecord>, AuthError>> {
        Box::pin(async move {
            let guard = self.records.read().await;
            let mut list: Vec<ApiKeyRecord> = guard
                .values()
                .filter(|r| r.user_id == user_id)
                .cloned()
                .collect();
            list.sort_by_key(|b| std::cmp::Reverse(b.created_at));
            Ok(list)
        })
    }

    fn touch_last_used<'a>(
        &'a self,
        id: &'a str,
        now_epoch_sec: u64,
    ) -> BoxFuture<'a, Result<(), AuthError>> {
        Box::pin(async move {
            if let Some(record) = self.records.write().await.get_mut(id) {
                record.last_used_at = Some(now_epoch_sec);
            }
            Ok(())
        })
    }
}

/// SQL-backed [`ApiKeyStore`] using `rume-data` queries.
#[derive(Debug, Clone)]
pub struct SqlApiKeyStore<B: DatabaseBackend> {
    backend: Arc<B>,
}

impl<B: DatabaseBackend> SqlApiKeyStore<B> {
    /// Creates a new [`SqlApiKeyStore`] backed by `backend`.
    pub fn new(backend: Arc<B>) -> Self {
        Self { backend }
    }
}

impl<B: DatabaseBackend> ApiKeyStore for SqlApiKeyStore<B> {
    fn create<'a>(
        &'a self,
        user_id: &'a str,
        tenant_id: Option<&'a str>,
        name: &'a str,
        scopes: Vec<String>,
        expires_at: Option<u64>,
        now_epoch_sec: u64,
    ) -> BoxFuture<'a, Result<IssuedApiKey, AuthError>> {
        Box::pin(async move {
            let issued =
                generate_api_key(user_id, tenant_id, name, scopes, expires_at, now_epoch_sec);
            let params = insert_params(&issued.record)?;
            let stmt = select_sql(
                self.backend.dialect(),
                sql::PG_AUTH_API_KEY_INSERT,
                sql::MYSQL_AUTH_API_KEY_INSERT,
                sql::SQLITE_AUTH_API_KEY_INSERT,
            );
            self.backend
                .execute_params(stmt, &params)
                .await
                .map_err(|e| AuthError::ProviderError(e.to_string()))?;
            Ok(issued)
        })
    }

    fn find_by_prefix<'a>(
        &'a self,
        prefix: &'a str,
    ) -> BoxFuture<'a, Result<Vec<ApiKeyRecord>, AuthError>> {
        Box::pin(async move {
            let stmt = select_sql(
                self.backend.dialect(),
                sql::PG_AUTH_API_KEY_FIND_BY_PREFIX,
                sql::MYSQL_AUTH_API_KEY_FIND_BY_PREFIX,
                sql::SQLITE_AUTH_API_KEY_FIND_BY_PREFIX,
            );
            let rows = self
                .backend
                .fetch_all(stmt, &[DbValue::from(prefix)])
                .await
                .map_err(|e| AuthError::ProviderError(e.to_string()))?;
            rows.iter().map(decode_api_key_row).collect()
        })
    }

    fn verify_key<'a>(
        &'a self,
        raw_key: &'a str,
        now_epoch_sec: u64,
    ) -> BoxFuture<'a, Result<Option<ApiKeyRecord>, AuthError>> {
        Box::pin(async move {
            let Some(prefix) = extract_lookup_prefix(raw_key) else {
                return Ok(None);
            };
            let candidates = self.find_by_prefix(prefix).await?;
            Ok(verify_against_candidates(
                candidates,
                raw_key,
                now_epoch_sec,
            ))
        })
    }

    fn revoke<'a>(
        &'a self,
        id: &'a str,
        now_epoch_sec: u64,
    ) -> BoxFuture<'a, Result<bool, AuthError>> {
        Box::pin(async move {
            let stmt = select_sql(
                self.backend.dialect(),
                sql::PG_AUTH_API_KEY_REVOKE,
                sql::MYSQL_AUTH_API_KEY_REVOKE,
                sql::SQLITE_AUTH_API_KEY_REVOKE,
            );
            let affected = self
                .backend
                .execute_params(stmt, &[DbValue::from(now_epoch_sec), DbValue::from(id)])
                .await
                .map_err(|e| AuthError::ProviderError(e.to_string()))?;
            Ok(affected > 0)
        })
    }

    fn rotate<'a>(
        &'a self,
        old_id: &'a str,
        new_expires_at: Option<u64>,
        now_epoch_sec: u64,
    ) -> BoxFuture<'a, Result<IssuedApiKey, AuthError>> {
        Box::pin(async move {
            let dialect = self.backend.dialect();
            let tx = self
                .backend
                .begin_immediate_transaction()
                .await
                .map_err(|e| AuthError::ProviderError(e.to_string()))?;

            let find_stmt = select_sql(
                dialect,
                sql::PG_AUTH_API_KEY_FIND_BY_ID,
                sql::MYSQL_AUTH_API_KEY_FIND_BY_ID,
                sql::SQLITE_AUTH_API_KEY_FIND_BY_ID,
            );
            let Some(row) = tx
                .fetch_optional(find_stmt, &[DbValue::from(old_id)])
                .await
                .map_err(|e| AuthError::ProviderError(e.to_string()))?
            else {
                let _ = tx.rollback().await;
                return Err(AuthError::InvalidCredentials("api key not found".into()));
            };

            let old = decode_api_key_row(&row)?;
            if old.revoked_at.is_some() {
                let _ = tx.rollback().await;
                return Err(AuthError::InvalidCredentials(
                    "api key already revoked".into(),
                ));
            }
            if old.expires_at.is_some_and(|exp| exp <= now_epoch_sec) {
                let _ = tx.rollback().await;
                return Err(AuthError::Expired);
            }

            let revoke_stmt = select_sql(
                dialect,
                sql::PG_AUTH_API_KEY_REVOKE,
                sql::MYSQL_AUTH_API_KEY_REVOKE,
                sql::SQLITE_AUTH_API_KEY_REVOKE,
            );
            let affected = tx
                .execute_params(
                    revoke_stmt,
                    &[DbValue::from(now_epoch_sec), DbValue::from(old_id)],
                )
                .await
                .map_err(|e| AuthError::ProviderError(e.to_string()))?;
            if affected != 1 {
                let _ = tx.rollback().await;
                return Err(AuthError::InvalidCredentials(
                    "api key already revoked".into(),
                ));
            }

            let issued = generate_api_key(
                &old.user_id,
                old.tenant_id.as_deref(),
                &old.name,
                old.scopes,
                new_expires_at,
                now_epoch_sec,
            );
            let params = insert_params(&issued.record)?;
            let insert_stmt = select_sql(
                dialect,
                sql::PG_AUTH_API_KEY_INSERT,
                sql::MYSQL_AUTH_API_KEY_INSERT,
                sql::SQLITE_AUTH_API_KEY_INSERT,
            );
            tx.execute_params(insert_stmt, &params)
                .await
                .map_err(|e| AuthError::ProviderError(e.to_string()))?;

            tx.commit()
                .await
                .map_err(|e| AuthError::ProviderError(e.to_string()))?;
            let _ = issued.plaintext.expose_secret();
            Ok(issued)
        })
    }

    fn list_for_user<'a>(
        &'a self,
        user_id: &'a str,
    ) -> BoxFuture<'a, Result<Vec<ApiKeyRecord>, AuthError>> {
        Box::pin(async move {
            let stmt = select_sql(
                self.backend.dialect(),
                sql::PG_AUTH_API_KEY_LIST_FOR_USER,
                sql::MYSQL_AUTH_API_KEY_LIST_FOR_USER,
                sql::SQLITE_AUTH_API_KEY_LIST_FOR_USER,
            );
            let rows = self
                .backend
                .fetch_all(stmt, &[DbValue::from(user_id)])
                .await
                .map_err(|e| AuthError::ProviderError(e.to_string()))?;
            rows.iter().map(decode_api_key_row).collect()
        })
    }

    fn touch_last_used<'a>(
        &'a self,
        id: &'a str,
        now_epoch_sec: u64,
    ) -> BoxFuture<'a, Result<(), AuthError>> {
        Box::pin(async move {
            let stmt = select_sql(
                self.backend.dialect(),
                sql::PG_AUTH_API_KEY_TOUCH_LAST_USED,
                sql::MYSQL_AUTH_API_KEY_TOUCH_LAST_USED,
                sql::SQLITE_AUTH_API_KEY_TOUCH_LAST_USED,
            );
            self.backend
                .execute_params(stmt, &[DbValue::from(now_epoch_sec), DbValue::from(id)])
                .await
                .map_err(|e| AuthError::ProviderError(e.to_string()))?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use secrecy::ExposeSecret;

    use super::*;
    use rume_data::persistence::{Migrator, SqliteBackend};

    async fn setup_sqlite() -> Arc<SqliteBackend> {
        let backend = Arc::new(SqliteBackend::in_memory().await.unwrap());
        let migrator = Migrator::new().with_rume_auth("sqlite");
        migrator.run_pending(backend.as_ref()).await.unwrap();
        backend
    }

    #[tokio::test]
    async fn sql_api_key_store_create_verify_revoke_and_expiry_on_sqlite() {
        let backend = setup_sqlite().await;
        let store = SqlApiKeyStore::new(backend);

        let issued = store
            .create(
                "user-1",
                Some("tenant-a"),
                "ci-deploy",
                vec!["deploy:write".into(), "logs:read".into()],
                Some(2_000),
                1_000,
            )
            .await
            .unwrap();

        let plaintext = issued.plaintext.expose_secret().to_string();
        assert!(plaintext.starts_with("rk_"));
        assert_eq!(issued.record.prefix, &plaintext[..12]);
        assert_eq!(issued.record.user_id, "user-1");
        assert_eq!(issued.record.tenant_id.as_deref(), Some("tenant-a"));
        assert_eq!(
            issued.record.scopes,
            vec!["deploy:write".to_string(), "logs:read".to_string()]
        );

        let verified = store.verify_key(&plaintext, 1_500).await.unwrap();
        assert!(verified.is_some());
        assert_eq!(verified.unwrap().id, issued.record.id);

        // Wrong secret with same prefix must fail verification.
        let mut wrong = plaintext.clone();
        let last = wrong.pop().unwrap();
        wrong.push(if last == 'A' { 'B' } else { 'A' });
        assert!(store.verify_key(&wrong, 1_500).await.unwrap().is_none());

        // Expired at expires_at.
        assert!(store.verify_key(&plaintext, 2_000).await.unwrap().is_none());

        // Touch last_used_at and list_for_user.
        store
            .touch_last_used(&issued.record.id, 1_600)
            .await
            .unwrap();
        let listed = store.list_for_user("user-1").await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].last_used_at, Some(1_600));

        // Revoke and verify rejected even before expiry.
        assert!(store.revoke(&issued.record.id, 1_700).await.unwrap());
        assert!(!store.revoke(&issued.record.id, 1_701).await.unwrap());
        assert!(store.verify_key(&plaintext, 1_750).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn sql_api_key_store_rotate_revokes_old_key_and_issues_new_key_atomically() {
        let backend = setup_sqlite().await;
        let store = SqlApiKeyStore::new(backend);

        let initial = store
            .create(
                "user-2",
                Some("tenant-b"),
                "primary-key",
                vec!["read".into()],
                Some(5_000),
                1_000,
            )
            .await
            .unwrap();

        let rotated = store
            .rotate(&initial.record.id, Some(9_000), 2_000)
            .await
            .unwrap();

        assert_ne!(rotated.record.id, initial.record.id);
        assert_eq!(rotated.record.user_id, "user-2");
        assert_eq!(rotated.record.tenant_id.as_deref(), Some("tenant-b"));
        assert_eq!(rotated.record.scopes, vec!["read".to_string()]);
        assert_eq!(rotated.record.expires_at, Some(9_000));

        // Old key is revoked immediately; new key verifies.
        assert!(
            store
                .verify_key(initial.plaintext.expose_secret(), 2_500)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .verify_key(rotated.plaintext.expose_secret(), 2_500)
                .await
                .unwrap()
                .is_some()
        );

        // Rotating an already-revoked key fails.
        assert!(
            store
                .rotate(&initial.record.id, Some(9_999), 2_600)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn in_memory_api_key_store_rotate_rejects_expired_key() {
        let store = InMemoryApiKeyStore::new();
        let issued = store
            .create(
                "user-expired",
                Some("tenant-a"),
                "expiring-key",
                vec!["read".into()],
                Some(2_000),
                1_000,
            )
            .await
            .unwrap();

        assert_eq!(
            store
                .rotate(&issued.record.id, Some(9_000), 2_000)
                .await
                .unwrap_err(),
            AuthError::Expired
        );
        assert_eq!(
            store
                .rotate(&issued.record.id, Some(9_000), 2_001)
                .await
                .unwrap_err(),
            AuthError::Expired
        );
        // No replacement key should have been created.
        assert_eq!(store.list_for_user("user-expired").await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn sql_api_key_store_rotate_rejects_expired_key_and_rolls_back() {
        let backend = setup_sqlite().await;
        let store = SqlApiKeyStore::new(backend);
        let issued = store
            .create(
                "user-expired",
                Some("tenant-a"),
                "expiring-key",
                vec!["read".into()],
                Some(2_000),
                1_000,
            )
            .await
            .unwrap();

        assert_eq!(
            store
                .rotate(&issued.record.id, Some(9_000), 2_000)
                .await
                .unwrap_err(),
            AuthError::Expired
        );
        assert_eq!(
            store
                .rotate(&issued.record.id, Some(9_000), 2_001)
                .await
                .unwrap_err(),
            AuthError::Expired
        );
        // Transaction rolled back: no replacement key created and original key not revoked.
        let keys = store.list_for_user("user-expired").await.unwrap();
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].revoked_at, None);
    }
}
