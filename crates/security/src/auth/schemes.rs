//! Pluggable authentication schemes (Bearer tokens, API keys, session cookies).

use std::collections::HashMap;
use std::sync::Arc;

use axum::http::header::{AUTHORIZATION, COOKIE};
use axum::http::request::Parts;
use sha2::{Digest, Sha256};

use crate::auth::error::AuthError;
use crate::auth::identity::AuthUser;
use crate::auth::jwt::{JwtClaims, JwtVerifier};
use crate::auth::session::{BoxFuture, SessionStore};

/// An authentication scheme that extracts and verifies credentials from request parts.
pub trait AuthenticationScheme: Send + Sync + 'static {
    /// Attempts to authenticate the request, returning an [`AuthUser`] if successful.
    ///
    /// If no relevant credentials exist in the request, returns `Ok(None)` so the next scheme can try.
    /// If invalid credentials were provided, returns `Err(AuthError)`.
    fn authenticate<'a>(
        &'a self,
        parts: &'a Parts,
    ) -> BoxFuture<'a, Result<Option<AuthUser>, AuthError>>;
}

/// Token validator function type.
pub type TokenValidator = Arc<
    dyn Fn(&str) -> BoxFuture<'static, Result<Option<AuthUser>, AuthError>> + Send + Sync + 'static,
>;

/// Maps verified [`JwtClaims`] into an [`AuthUser`].
#[derive(Clone)]
pub struct JwtClaimsMapper {
    mapper: Arc<dyn Fn(JwtClaims) -> Result<AuthUser, AuthError> + Send + Sync + 'static>,
}

impl std::fmt::Debug for JwtClaimsMapper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JwtClaimsMapper").finish_non_exhaustive()
    }
}

impl Default for JwtClaimsMapper {
    fn default() -> Self {
        Self::new(JwtClaims::into_auth_user)
    }
}

impl JwtClaimsMapper {
    /// Creates a custom claims-to-user mapper.
    pub fn new<F>(mapper: F) -> Self
    where
        F: Fn(JwtClaims) -> Result<AuthUser, AuthError> + Send + Sync + 'static,
    {
        Self {
            mapper: Arc::new(mapper),
        }
    }

    /// Maps verified [`JwtClaims`] into an [`AuthUser`].
    pub fn map(&self, claims: JwtClaims) -> Result<AuthUser, AuthError> {
        (self.mapper)(claims)
    }
}

/// Bearer token authentication scheme (`Authorization: Bearer <token>`).
#[derive(Clone)]
pub struct BearerTokenScheme {
    validator: TokenValidator,
}

impl BearerTokenScheme {
    /// Creates a new Bearer token scheme with a token verification closure.
    pub fn new<F, Fut>(validator: F) -> Self
    where
        F: Fn(String) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<Option<AuthUser>, AuthError>> + Send + 'static,
    {
        Self {
            validator: Arc::new(move |token| {
                let token_owned = token.to_string();
                let fut = validator(token_owned);
                Box::pin(fut)
            }),
        }
    }

    /// Creates a Bearer token scheme backed by a [`JwtVerifier`] and [`JwtClaimsMapper`].
    pub fn from_jwt_verifier(verifier: JwtVerifier, mapper: JwtClaimsMapper) -> Self {
        let verifier = Arc::new(verifier);
        Self {
            validator: Arc::new(move |token| {
                let verifier = Arc::clone(&verifier);
                let mapper = mapper.clone();
                let token = token.to_owned();
                Box::pin(async move {
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();
                    let claims = verifier.verify(&token, now).await?;
                    let user = mapper.map(claims)?;
                    Ok(Some(user))
                })
            }),
        }
    }
}

impl AuthenticationScheme for BearerTokenScheme {
    fn authenticate<'a>(
        &'a self,
        parts: &'a Parts,
    ) -> BoxFuture<'a, Result<Option<AuthUser>, AuthError>> {
        Box::pin(async move {
            let Some(auth_header) = parts.headers.get(AUTHORIZATION) else {
                return Ok(None);
            };

            let Ok(auth_str) = auth_header.to_str() else {
                return Err(AuthError::InvalidCredentials(
                    "malformed authorization header".into(),
                ));
            };

            if let Some(token) = auth_str.strip_prefix("Bearer ").map(str::trim) {
                if token.is_empty() {
                    return Err(AuthError::InvalidCredentials("empty bearer token".into()));
                }
                (self.validator)(token).await
            } else {
                Ok(None)
            }
        })
    }
}

/// API Key authentication scheme (e.g. `X-Api-Key: <key>`).
#[derive(Clone)]
pub struct ApiKeyScheme {
    header_name: String,
    keys: Arc<HashMap<[u8; 32], AuthUser>>,
    store: Option<Arc<dyn crate::auth::api_key::ApiKeyStore>>,
}

impl ApiKeyScheme {
    /// Creates a new API key scheme reading from `X-Api-Key` header.
    pub fn new() -> Self {
        Self {
            header_name: "x-api-key".to_string(),
            keys: Arc::new(HashMap::new()),
            store: None,
        }
    }

    /// Sets the custom header name for the API key.
    pub fn with_header(mut self, header: impl Into<String>) -> Self {
        self.header_name = header.into().to_lowercase();
        self
    }

    /// Registers a known API key mapped to an [`AuthUser`].
    ///
    /// Cryptographically hashes the key using SHA-256 before storing it,
    /// preventing plaintext keys from sitting in heap memory.
    pub fn add_key(mut self, key: impl AsRef<[u8]>, user: AuthUser) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(key.as_ref());
        let hash: [u8; 32] = hasher.finalize().into();
        Arc::make_mut(&mut self.keys).insert(hash, user);
        self
    }

    /// Backs this scheme with a persistent [`ApiKeyStore`](crate::auth::api_key::ApiKeyStore).
    pub fn with_store(mut self, store: impl crate::auth::api_key::ApiKeyStore) -> Self {
        self.store = Some(Arc::new(store));
        self
    }
}

impl Default for ApiKeyScheme {
    fn default() -> Self {
        Self::new()
    }
}

impl AuthenticationScheme for ApiKeyScheme {
    fn authenticate<'a>(
        &'a self,
        parts: &'a Parts,
    ) -> BoxFuture<'a, Result<Option<AuthUser>, AuthError>> {
        Box::pin(async move {
            let Some(key_header) = parts.headers.get(&self.header_name) else {
                return Ok(None);
            };

            let Ok(key_str) = key_header.to_str() else {
                return Err(AuthError::InvalidCredentials(
                    "malformed api key header".into(),
                ));
            };

            let mut hasher = Sha256::new();
            hasher.update(key_str.as_bytes());
            let incoming_hash: [u8; 32] = hasher.finalize().into();

            if let Some(user) = self.keys.get(&incoming_hash) {
                return Ok(Some(user.clone()));
            }

            if let Some(store) = &self.store {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                if let Some(record) = store.verify_key(key_str, now).await? {
                    let _ = store.touch_last_used(&record.id, now).await;
                    let mut user = AuthUser::new(record.user_id).with_permissions(record.scopes);
                    if let Some(tenant_id) = record.tenant_id {
                        user = user.with_tenant_id(tenant_id);
                    }
                    return Ok(Some(user));
                }
            }

            Err(AuthError::InvalidCredentials("unrecognized api key".into()))
        })
    }
}

/// Cookie-based session authentication scheme.
#[derive(Clone)]
pub struct SessionScheme<S: SessionStore> {
    cookie_name: String,
    store: Arc<S>,
}

impl<S: SessionStore> SessionScheme<S> {
    /// Creates a new session scheme with the given session store.
    pub fn new(store: S) -> Self {
        Self {
            cookie_name: "rume_session".to_string(),
            store: Arc::new(store),
        }
    }

    /// Sets the custom session cookie name.
    pub fn with_cookie_name(mut self, name: impl Into<String>) -> Self {
        self.cookie_name = name.into();
        self
    }
}

/// Maximum number of matching session cookie candidates queried against the store per request.
///
/// Why: bounds per-request store lookups against cookie-flooding DoS while still tolerating
/// legitimate stale/tossed duplicate cookies.
const MAX_SESSION_COOKIE_CANDIDATES: usize = 8;

impl<S: SessionStore> AuthenticationScheme for SessionScheme<S> {
    fn authenticate<'a>(
        &'a self,
        parts: &'a Parts,
    ) -> BoxFuture<'a, Result<Option<AuthUser>, AuthError>> {
        Box::pin(async move {
            // Security M2: Iterate through all Cookie headers and all matching cookie pairs,
            // capped at `MAX_SESSION_COOKIE_CANDIDATES` store lookups per request.
            // Continuing past invalid/stale cookies defeats subdomain cookie-tossing DoS,
            // where an attacker sets a bogus parent-domain cookie with the same name.
            let mut candidates_checked = 0usize;
            for cookie_header in parts.headers.get_all(COOKIE) {
                let Ok(cookie_str) = cookie_header.to_str() else {
                    continue;
                };

                for cookie in cookie_str.split(';') {
                    if let Some((name, val)) = cookie.trim().split_once('=')
                        && name == self.cookie_name
                    {
                        if candidates_checked >= MAX_SESSION_COOKIE_CANDIDATES {
                            return Ok(None);
                        }
                        candidates_checked += 1;
                        match self.store.load(val).await {
                            Ok(Some(session)) => {
                                let mut user = AuthUser::new(session.user_id)
                                    .with_roles(session.roles)
                                    .with_permissions(session.permissions);
                                if let Some(tenant_id) = &session.tenant_id {
                                    user = user.with_tenant_id(tenant_id.clone());
                                }
                                user.metadata = session.data;
                                return Ok(Some(user));
                            }
                            Ok(None) => {
                                // Stale or tossed cookie: continue checking subsequent cookies
                                continue;
                            }
                            Err(err) => {
                                // Infrastructure or provider error: do not silently degrade to guest!
                                return Err(err);
                            }
                        }
                    }
                }
            }

            Ok(None)
        })
    }
}

#[cfg(test)]
#[path = "schemes_tests.rs"]
mod tests;
