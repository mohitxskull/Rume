//! Persistent API key management (`ApiKeyStore`, `SqlApiKeyStore`, `InMemoryApiKeyStore`).

#![allow(dead_code, unused_variables)]

use std::sync::Arc;

use rume_data::persistence::DatabaseBackend;
use secrecy::SecretString;

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

/// In-memory [`ApiKeyStore`] for tests and single-node setups.
#[derive(Debug, Default)]
pub struct InMemoryApiKeyStore {
    _private: (),
}

impl InMemoryApiKeyStore {
    /// Creates an empty in-memory API key store.
    pub fn new() -> Self {
        Self { _private: () }
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
        todo!("implemented in Task 04")
    }

    fn find_by_prefix<'a>(
        &'a self,
        prefix: &'a str,
    ) -> BoxFuture<'a, Result<Vec<ApiKeyRecord>, AuthError>> {
        todo!("implemented in Task 04")
    }

    fn verify_key<'a>(
        &'a self,
        raw_key: &'a str,
        now_epoch_sec: u64,
    ) -> BoxFuture<'a, Result<Option<ApiKeyRecord>, AuthError>> {
        todo!("implemented in Task 04")
    }

    fn revoke<'a>(
        &'a self,
        id: &'a str,
        now_epoch_sec: u64,
    ) -> BoxFuture<'a, Result<bool, AuthError>> {
        todo!("implemented in Task 04")
    }

    fn rotate<'a>(
        &'a self,
        old_id: &'a str,
        new_expires_at: Option<u64>,
        now_epoch_sec: u64,
    ) -> BoxFuture<'a, Result<IssuedApiKey, AuthError>> {
        todo!("implemented in Task 04")
    }

    fn list_for_user<'a>(
        &'a self,
        user_id: &'a str,
    ) -> BoxFuture<'a, Result<Vec<ApiKeyRecord>, AuthError>> {
        todo!("implemented in Task 04")
    }

    fn touch_last_used<'a>(
        &'a self,
        id: &'a str,
        now_epoch_sec: u64,
    ) -> BoxFuture<'a, Result<(), AuthError>> {
        todo!("implemented in Task 04")
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
        todo!("implemented in Task 04")
    }

    fn find_by_prefix<'a>(
        &'a self,
        prefix: &'a str,
    ) -> BoxFuture<'a, Result<Vec<ApiKeyRecord>, AuthError>> {
        todo!("implemented in Task 04")
    }

    fn verify_key<'a>(
        &'a self,
        raw_key: &'a str,
        now_epoch_sec: u64,
    ) -> BoxFuture<'a, Result<Option<ApiKeyRecord>, AuthError>> {
        todo!("implemented in Task 04")
    }

    fn revoke<'a>(
        &'a self,
        id: &'a str,
        now_epoch_sec: u64,
    ) -> BoxFuture<'a, Result<bool, AuthError>> {
        todo!("implemented in Task 04")
    }

    fn rotate<'a>(
        &'a self,
        old_id: &'a str,
        new_expires_at: Option<u64>,
        now_epoch_sec: u64,
    ) -> BoxFuture<'a, Result<IssuedApiKey, AuthError>> {
        todo!("implemented in Task 04")
    }

    fn list_for_user<'a>(
        &'a self,
        user_id: &'a str,
    ) -> BoxFuture<'a, Result<Vec<ApiKeyRecord>, AuthError>> {
        todo!("implemented in Task 04")
    }

    fn touch_last_used<'a>(
        &'a self,
        id: &'a str,
        now_epoch_sec: u64,
    ) -> BoxFuture<'a, Result<(), AuthError>> {
        todo!("implemented in Task 04")
    }
}
