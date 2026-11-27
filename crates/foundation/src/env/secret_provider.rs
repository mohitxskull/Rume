//! Dynamic secret provider abstractions for Rume.
//!
//! Provides [`SecretProvider`] for fetching and rotating secrets at runtime,
//! [`EnvSecretProvider`] for environment-backed lookups, [`CachedSecretProvider`]
//! for TTL-bounded in-memory caching with rotation callbacks, and
//! [`ExternalSecretBackend`] hooks for Vault and KMS integrations.

use std::collections::BTreeMap;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use parking_lot::RwLock;
use secrecy::SecretString;
use thiserror::Error;
use tokio::time::Instant;

/// Pinned boxed future returned by async [`SecretProvider`] operations.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Callback invoked when a secret is rotated via [`CachedSecretProvider::rotate`].
pub type RotationCallback = Arc<dyn Fn(&str) + Send + Sync>;

/// Errors returned by [`SecretProvider`] and [`ExternalSecretBackend`] implementations.
///
/// Error messages never include secret values.
#[derive(Debug, Error)]
pub enum SecretProviderError {
    /// The requested secret key was not found.
    #[error("secret '{key}' not found")]
    NotFound { key: String },

    /// The provider does not support rotating the requested secret.
    #[error("secret rotation is not supported for '{key}'")]
    RotationUnsupported { key: String },

    /// An underlying backend or transport error occurred.
    #[error("secret backend error: {0}")]
    Backend(String),
}

/// Dynamic secret retrieval and rotation contract.
pub trait SecretProvider: Send + Sync + 'static {
    /// Fetches the current value for `key`.
    fn get_secret<'a>(
        &'a self,
        key: &'a str,
    ) -> BoxFuture<'a, Result<SecretString, SecretProviderError>>;

    /// Rotates `key` in the backing store and returns the new secret value.
    fn rotate<'a>(
        &'a self,
        key: &'a str,
    ) -> BoxFuture<'a, Result<SecretString, SecretProviderError>>;
}

#[derive(Clone)]
enum EnvSource {
    System,
    Map(BTreeMap<String, SecretString>),
}

/// [`SecretProvider`] backed by process environment variables or an explicit
/// in-memory map for hermetic testing.
///
/// `Debug` output never reveals secret values.
#[derive(Clone)]
pub struct EnvSecretProvider {
    source: EnvSource,
}

impl EnvSecretProvider {
    /// Creates a provider that reads from `std::env::var`.
    pub fn system() -> Self {
        Self {
            source: EnvSource::System,
        }
    }

    /// Creates a hermetic provider backed by an in-memory map of key/secret pairs.
    pub fn from_map<I, K, V>(entries: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<SecretString>,
    {
        let map = entries
            .into_iter()
            .map(|(k, v)| (k.into(), v.into()))
            .collect();
        Self {
            source: EnvSource::Map(map),
        }
    }
}

impl Default for EnvSecretProvider {
    fn default() -> Self {
        Self::system()
    }
}

impl fmt::Debug for EnvSecretProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.source {
            EnvSource::System => f
                .debug_struct("EnvSecretProvider")
                .field("source", &"system")
                .finish(),
            EnvSource::Map(map) => {
                let keys: Vec<&str> = map.keys().map(String::as_str).collect();
                f.debug_struct("EnvSecretProvider")
                    .field("source", &"map")
                    .field("keys", &keys)
                    .finish()
            }
        }
    }
}

impl SecretProvider for EnvSecretProvider {
    fn get_secret<'a>(
        &'a self,
        key: &'a str,
    ) -> BoxFuture<'a, Result<SecretString, SecretProviderError>> {
        Box::pin(async move {
            match &self.source {
                EnvSource::System => {
                    let val = std::env::var(key).map_err(|_| SecretProviderError::NotFound {
                        key: key.to_string(),
                    })?;
                    Ok(SecretString::from(val))
                }
                EnvSource::Map(map) => {
                    map.get(key)
                        .cloned()
                        .ok_or_else(|| SecretProviderError::NotFound {
                            key: key.to_string(),
                        })
                }
            }
        })
    }

    fn rotate<'a>(
        &'a self,
        key: &'a str,
    ) -> BoxFuture<'a, Result<SecretString, SecretProviderError>> {
        Box::pin(async move {
            Err(SecretProviderError::RotationUnsupported {
                key: key.to_string(),
            })
        })
    }
}

#[derive(Clone)]
struct CachedEntry {
    value: SecretString,
    expires_at: Instant,
}

impl fmt::Debug for CachedEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CachedEntry")
            .field("value", &"[redacted]")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

/// TTL-caching decorator around any [`SecretProvider`] with explicit
/// invalidation and rotation callback hooks.
///
/// `Debug` output redacts all cached secret values.
pub struct CachedSecretProvider {
    inner: Arc<dyn SecretProvider>,
    ttl: Duration,
    cache: RwLock<BTreeMap<String, CachedEntry>>,
    generation: AtomicU64,
    on_rotation: Option<RotationCallback>,
}

impl CachedSecretProvider {
    /// Wraps `inner` with a cache whose entries expire after `ttl`.
    pub fn new(inner: Arc<dyn SecretProvider>, ttl: Duration) -> Self {
        Self {
            inner,
            ttl,
            cache: RwLock::new(BTreeMap::new()),
            generation: AtomicU64::new(0),
            on_rotation: None,
        }
    }

    /// Registers a callback invoked with the secret key whenever [`Self::rotate`]
    /// succeeds.
    pub fn with_on_rotation(mut self, callback: impl Fn(&str) + Send + Sync + 'static) -> Self {
        self.on_rotation = Some(Arc::new(callback));
        self
    }

    /// Evicts a single cached secret so the next [`Self::get_secret`] reloads it.
    pub fn invalidate(&self, key: &str) {
        let mut cache = self.cache.write();
        self.generation.fetch_add(1, Ordering::AcqRel);
        cache.remove(key);
    }

    /// Clears all cached secrets.
    pub fn invalidate_all(&self) {
        let mut cache = self.cache.write();
        self.generation.fetch_add(1, Ordering::AcqRel);
        cache.clear();
    }
}

impl fmt::Debug for CachedSecretProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let keys: Vec<String> = self.cache.read().keys().cloned().collect();
        f.debug_struct("CachedSecretProvider")
            .field("ttl", &self.ttl)
            .field("cached_keys", &keys)
            .field("has_on_rotation", &self.on_rotation.is_some())
            .finish()
    }
}

impl SecretProvider for CachedSecretProvider {
    fn get_secret<'a>(
        &'a self,
        key: &'a str,
    ) -> BoxFuture<'a, Result<SecretString, SecretProviderError>> {
        Box::pin(async move {
            let now = Instant::now();
            if !self.ttl.is_zero()
                && let Some(entry) = self.cache.read().get(key)
                && now < entry.expires_at
            {
                return Ok(entry.value.clone());
            }

            // Why: Capture generation before awaiting `inner.get_secret` so an in-flight fetch
            // returning a pre-rotation secret cannot overwrite a newer rotated or invalidated
            // entry once it completes (`RC-18`).
            let observed_gen = self.generation.load(Ordering::Acquire);
            let secret = self.inner.get_secret(key).await?;
            if !self.ttl.is_zero() {
                let mut cache = self.cache.write();
                if self.generation.load(Ordering::Acquire) == observed_gen
                    && let Some(expires_at) = Instant::now().checked_add(self.ttl)
                {
                    cache.insert(
                        key.to_string(),
                        CachedEntry {
                            value: secret.clone(),
                            expires_at,
                        },
                    );
                }
            }
            Ok(secret)
        })
    }

    fn rotate<'a>(
        &'a self,
        key: &'a str,
    ) -> BoxFuture<'a, Result<SecretString, SecretProviderError>> {
        Box::pin(async move {
            match self.inner.rotate(key).await {
                Ok(new_secret) => {
                    {
                        let mut cache = self.cache.write();
                        self.generation.fetch_add(1, Ordering::AcqRel);
                        if !self.ttl.is_zero()
                            && let Some(expires_at) = Instant::now().checked_add(self.ttl)
                        {
                            cache.insert(
                                key.to_string(),
                                CachedEntry {
                                    value: new_secret.clone(),
                                    expires_at,
                                },
                            );
                        } else {
                            cache.remove(key);
                        }
                    }
                    if let Some(cb) = &self.on_rotation {
                        cb(key);
                    }
                    Ok(new_secret)
                }
                Err(err) => {
                    self.invalidate(key);
                    Err(err)
                }
            }
        })
    }
}

/// Pluggable backend hook for external secret managers (HashiCorp Vault,
/// AWS KMS / Secrets Manager, GCP Secret Manager) without coupling
/// `rume-foundation` to a vendor SDK.
pub trait ExternalSecretBackend: Send + Sync + 'static {
    /// Human-readable backend name (e.g. `"vault"`, `"kms"`).
    fn name(&self) -> &str;

    /// Fetches a secret at `path` from the external backend.
    fn fetch_secret<'a>(
        &'a self,
        path: &'a str,
    ) -> BoxFuture<'a, Result<SecretString, SecretProviderError>>;

    /// Rotates a secret at `path` in the external backend.
    fn rotate_secret<'a>(
        &'a self,
        path: &'a str,
    ) -> BoxFuture<'a, Result<SecretString, SecretProviderError>> {
        Box::pin(async move {
            Err(SecretProviderError::RotationUnsupported {
                key: path.to_string(),
            })
        })
    }
}

/// HashiCorp Vault adapter implementing [`SecretProvider`] via an
/// [`ExternalSecretBackend`].
pub struct VaultSecretProvider {
    backend: Arc<dyn ExternalSecretBackend>,
    mount: String,
    namespace: Option<String>,
}

impl VaultSecretProvider {
    /// Creates a Vault provider rooted at `mount` (for example `"secret/data"`).
    pub fn new(backend: Arc<dyn ExternalSecretBackend>, mount: impl Into<String>) -> Self {
        Self {
            backend,
            mount: mount.into(),
            namespace: None,
        }
    }

    /// Sets an optional Vault Enterprise namespace prefix.
    pub fn with_namespace(mut self, namespace: impl Into<String>) -> Self {
        self.namespace = Some(namespace.into());
        self
    }

    fn resolve_path(&self, key: &str) -> Result<String, SecretProviderError> {
        // Why: reject path traversal segments (`.`, `..`), backslashes, NUL bytes,
        // query strings, and fragments so untrusted secret keys cannot escape the
        // configured Vault mount or namespace (`RC-36`).
        if key.is_empty()
            || key.chars().any(|c| matches!(c, '\\' | '\0' | '?' | '#'))
            || key.split('/').any(|seg| seg == "." || seg == "..")
        {
            return Err(SecretProviderError::Backend(format!(
                "invalid Vault secret key path: '{key}'"
            )));
        }
        let mount = self.mount.trim_end_matches('/');
        let trimmed_key = key.trim_start_matches('/');
        if trimmed_key.is_empty() {
            return Err(SecretProviderError::Backend(
                "invalid Vault secret key path: empty after trimming".to_string(),
            ));
        }
        Ok(match &self.namespace {
            Some(ns) => format!("{}/{mount}/{trimmed_key}", ns.trim_matches('/')),
            None => format!("{mount}/{trimmed_key}"),
        })
    }
}

impl fmt::Debug for VaultSecretProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VaultSecretProvider")
            .field("backend", &self.backend.name())
            .field("mount", &self.mount)
            .field("namespace", &self.namespace)
            .finish()
    }
}

impl SecretProvider for VaultSecretProvider {
    fn get_secret<'a>(
        &'a self,
        key: &'a str,
    ) -> BoxFuture<'a, Result<SecretString, SecretProviderError>> {
        Box::pin(async move {
            let path = self.resolve_path(key)?;
            self.backend.fetch_secret(&path).await
        })
    }

    fn rotate<'a>(
        &'a self,
        key: &'a str,
    ) -> BoxFuture<'a, Result<SecretString, SecretProviderError>> {
        Box::pin(async move {
            let path = self.resolve_path(key)?;
            self.backend.rotate_secret(&path).await
        })
    }
}

/// KMS-backed secret provider implementing [`SecretProvider`] via an
/// [`ExternalSecretBackend`] scoped to a KMS key ID / ARN.
pub struct KmsSecretProvider {
    backend: Arc<dyn ExternalSecretBackend>,
    key_id: String,
}

impl KmsSecretProvider {
    /// Creates a KMS provider bound to `key_id`.
    pub fn new(backend: Arc<dyn ExternalSecretBackend>, key_id: impl Into<String>) -> Self {
        Self {
            backend,
            key_id: key_id.into(),
        }
    }

    fn resolve_path(&self, key: &str) -> String {
        format!("{}:{key}", self.key_id)
    }
}

impl fmt::Debug for KmsSecretProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KmsSecretProvider")
            .field("backend", &self.backend.name())
            .field("key_id", &self.key_id)
            .finish()
    }
}

impl SecretProvider for KmsSecretProvider {
    fn get_secret<'a>(
        &'a self,
        key: &'a str,
    ) -> BoxFuture<'a, Result<SecretString, SecretProviderError>> {
        Box::pin(async move {
            let path = self.resolve_path(key);
            self.backend.fetch_secret(&path).await
        })
    }

    fn rotate<'a>(
        &'a self,
        key: &'a str,
    ) -> BoxFuture<'a, Result<SecretString, SecretProviderError>> {
        Box::pin(async move {
            let path = self.resolve_path(key);
            self.backend.rotate_secret(&path).await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::ExposeSecret;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct RotatingMockProvider {
        fetches: AtomicUsize,
        version: AtomicUsize,
    }

    impl RotatingMockProvider {
        fn new() -> Self {
            Self {
                fetches: AtomicUsize::new(0),
                version: AtomicUsize::new(1),
            }
        }
    }

    impl SecretProvider for RotatingMockProvider {
        fn get_secret<'a>(
            &'a self,
            key: &'a str,
        ) -> BoxFuture<'a, Result<SecretString, SecretProviderError>> {
            Box::pin(async move {
                self.fetches.fetch_add(1, Ordering::SeqCst);
                let v = self.version.load(Ordering::SeqCst);
                Ok(SecretString::from(format!("{key}-v{v}-supersecret")))
            })
        }

        fn rotate<'a>(
            &'a self,
            key: &'a str,
        ) -> BoxFuture<'a, Result<SecretString, SecretProviderError>> {
            Box::pin(async move {
                let v = self.version.fetch_add(1, Ordering::SeqCst) + 1;
                Ok(SecretString::from(format!("{key}-v{v}-supersecret")))
            })
        }
    }

    #[tokio::test(start_paused = true)]
    async fn cached_secret_provider_respects_ttl_and_invokes_rotation_callback() {
        let inner = Arc::new(RotatingMockProvider::new());
        let rotated_keys = Arc::new(RwLock::new(Vec::<String>::new()));
        let rotated_keys_cb = Arc::clone(&rotated_keys);

        let provider = CachedSecretProvider::new(inner.clone(), Duration::from_millis(40))
            .with_on_rotation(move |key| {
                rotated_keys_cb.write().push(key.to_string());
            });

        // First fetch hits inner provider.
        let s1 = provider.get_secret("db_pass").await.unwrap();
        assert_eq!(s1.expose_secret(), "db_pass-v1-supersecret");
        assert_eq!(inner.fetches.load(Ordering::SeqCst), 1);

        // Second fetch within TTL hits cache without calling inner.
        let s2 = provider.get_secret("db_pass").await.unwrap();
        assert_eq!(s2.expose_secret(), "db_pass-v1-supersecret");
        assert_eq!(inner.fetches.load(Ordering::SeqCst), 1);

        // Debug output must never leak secret values.
        let debug_repr = format!("{provider:?}");
        assert!(!debug_repr.contains("supersecret"));
        assert!(debug_repr.contains("db_pass"));

        // Explicit invalidation forces reload.
        provider.invalidate("db_pass");
        let s3 = provider.get_secret("db_pass").await.unwrap();
        assert_eq!(s3.expose_secret(), "db_pass-v1-supersecret");
        assert_eq!(inner.fetches.load(Ordering::SeqCst), 2);

        // Advancing paused Tokio clock past TTL expires cache deterministically.
        tokio::time::advance(Duration::from_millis(55)).await;
        let s4 = provider.get_secret("db_pass").await.unwrap();
        assert_eq!(s4.expose_secret(), "db_pass-v1-supersecret");
        assert_eq!(inner.fetches.load(Ordering::SeqCst), 3);

        // Rotation updates cached secret and invokes rotation callback.
        let rotated = provider.rotate("db_pass").await.unwrap();
        assert_eq!(rotated.expose_secret(), "db_pass-v2-supersecret");
        assert_eq!(rotated_keys.read().as_slice(), &["db_pass".to_string()]);

        // Cached read after rotation returns the rotated value without extra fetch.
        let s5 = provider.get_secret("db_pass").await.unwrap();
        assert_eq!(s5.expose_secret(), "db_pass-v2-supersecret");
        assert_eq!(inner.fetches.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn env_secret_provider_and_external_hooks_work_without_leaking_debug() {
        let env_provider = EnvSecretProvider::from_map([("API_KEY", "secret-val-123")]);
        assert!(!format!("{env_provider:?}").contains("secret-val-123"));
        let val = env_provider.get_secret("API_KEY").await.unwrap();
        assert_eq!(val.expose_secret(), "secret-val-123");
        assert!(matches!(
            env_provider.get_secret("MISSING").await,
            Err(SecretProviderError::NotFound { .. })
        ));
        assert!(matches!(
            env_provider.rotate("API_KEY").await,
            Err(SecretProviderError::RotationUnsupported { .. })
        ));

        struct MapBackend(BTreeMap<String, String>);
        impl ExternalSecretBackend for MapBackend {
            fn name(&self) -> &str {
                "mock-vault"
            }

            fn fetch_secret<'a>(
                &'a self,
                path: &'a str,
            ) -> BoxFuture<'a, Result<SecretString, SecretProviderError>> {
                Box::pin(async move {
                    self.0
                        .get(path)
                        .cloned()
                        .map(SecretString::from)
                        .ok_or_else(|| SecretProviderError::NotFound {
                            key: path.to_string(),
                        })
                })
            }
        }

        let backend = Arc::new(MapBackend(BTreeMap::from([
            ("ns/secret/data/jwt".to_string(), "jwt-secret".to_string()),
            ("kms-key-1:db".to_string(), "kms-secret".to_string()),
        ])));

        let vault = VaultSecretProvider::new(backend.clone(), "secret/data").with_namespace("ns");
        assert_eq!(
            vault.get_secret("jwt").await.unwrap().expose_secret(),
            "jwt-secret"
        );

        let kms = KmsSecretProvider::new(backend, "kms-key-1");
        assert_eq!(
            kms.get_secret("db").await.unwrap().expose_secret(),
            "kms-secret"
        );
    }

    #[tokio::test]
    async fn cached_secret_provider_stale_fetch_does_not_overwrite_rotated_secret() {
        use tokio::sync::Barrier;

        struct BarrierProvider {
            fetch_started: Arc<Barrier>,
            rotate_finished: Arc<Barrier>,
        }

        impl SecretProvider for BarrierProvider {
            fn get_secret<'a>(
                &'a self,
                _key: &'a str,
            ) -> BoxFuture<'a, Result<SecretString, SecretProviderError>> {
                Box::pin(async move {
                    // Signal that get_secret is in flight (captured generation before inner fetch).
                    self.fetch_started.wait().await;
                    // Wait until rotate has completed and cached v2.
                    self.rotate_finished.wait().await;
                    Ok(SecretString::from("v1"))
                })
            }

            fn rotate<'a>(
                &'a self,
                _key: &'a str,
            ) -> BoxFuture<'a, Result<SecretString, SecretProviderError>> {
                Box::pin(async move { Ok(SecretString::from("v2")) })
            }
        }

        let fetch_started = Arc::new(Barrier::new(2));
        let rotate_finished = Arc::new(Barrier::new(2));
        let inner = Arc::new(BarrierProvider {
            fetch_started: Arc::clone(&fetch_started),
            rotate_finished: Arc::clone(&rotate_finished),
        });
        let provider = Arc::new(CachedSecretProvider::new(inner, Duration::from_secs(60)));

        let p_get = Arc::clone(&provider);
        let get_task = tokio::spawn(async move { p_get.get_secret("db_pass").await.unwrap() });

        let p_rot = Arc::clone(&provider);
        let rot_task = tokio::spawn(async move {
            // Wait until get_secret has entered inner.get_secret.
            fetch_started.wait().await;
            let rotated = p_rot.rotate("db_pass").await.unwrap();
            assert_eq!(rotated.expose_secret(), "v2");
            // Release get_secret so it returns stale v1.
            rotate_finished.wait().await;
        });

        let fetched = get_task.await.unwrap();
        assert_eq!(fetched.expose_secret(), "v1");
        rot_task.await.unwrap();

        // Subsequent cached read must return v2 (v1 must NOT have overwritten v2).
        let cached = provider.get_secret("db_pass").await.unwrap();
        assert_eq!(cached.expose_secret(), "v2");
    }

    #[tokio::test]
    async fn cached_secret_provider_duration_max_ttl_does_not_panic() {
        let inner = Arc::new(RotatingMockProvider::new());
        let provider = CachedSecretProvider::new(inner, Duration::MAX);
        let s1 = provider.get_secret("k").await.unwrap();
        assert_eq!(s1.expose_secret(), "k-v1-supersecret");
        let s2 = provider.rotate("k").await.unwrap();
        assert_eq!(s2.expose_secret(), "k-v2-supersecret");
    }

    #[tokio::test]
    async fn vault_secret_provider_rejects_path_traversal_and_unsafe_chars() {
        struct DenyAllBackend;
        impl ExternalSecretBackend for DenyAllBackend {
            fn name(&self) -> &str {
                "deny-all"
            }
            fn fetch_secret<'a>(
                &'a self,
                _path: &'a str,
            ) -> BoxFuture<'a, Result<SecretString, SecretProviderError>> {
                Box::pin(async { panic!("backend must not be called for invalid path") })
            }
        }

        let vault =
            VaultSecretProvider::new(Arc::new(DenyAllBackend), "secret/data").with_namespace("ns");

        for bad in [
            "",
            "/",
            ".",
            "..",
            "../admin/root",
            "a/../b",
            "a/./b",
            "a\\b",
            "a\0b",
            "a?version=1",
            "a#frag",
        ] {
            let err = vault.get_secret(bad).await.unwrap_err();
            assert!(
                matches!(err, SecretProviderError::Backend(_)),
                "expected Backend error for Vault key {bad:?}, got {err:?}"
            );
        }
    }
}
