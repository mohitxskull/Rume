//! Pluggable authentication schemes (Bearer tokens, API keys, session cookies).

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::http::header::{AUTHORIZATION, COOKIE};
use axum::http::request::Parts;

use crate::auth::error::AuthError;
use crate::auth::identity::AuthUser;
use crate::auth::session::{BoxFuture, SessionStore};
use crate::helpers::safe_equal;

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
    keys: Arc<BTreeMap<String, AuthUser>>,
}

impl ApiKeyScheme {
    /// Creates a new API key scheme reading from `X-Api-Key` header.
    pub fn new() -> Self {
        Self {
            header_name: "x-api-key".to_string(),
            keys: Arc::new(BTreeMap::new()),
        }
    }

    /// Sets the custom header name for the API key.
    pub fn with_header(mut self, header: impl Into<String>) -> Self {
        self.header_name = header.into().to_lowercase();
        self
    }

    /// Registers a known API key mapped to an [`AuthUser`].
    pub fn add_key(mut self, key: impl Into<String>, user: AuthUser) -> Self {
        Arc::make_mut(&mut self.keys).insert(key.into(), user);
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

            for (known_key, user) in self.keys.iter() {
                if safe_equal(key_str, known_key) {
                    return Ok(Some(user.clone()));
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

impl<S: SessionStore> AuthenticationScheme for SessionScheme<S> {
    fn authenticate<'a>(
        &'a self,
        parts: &'a Parts,
    ) -> BoxFuture<'a, Result<Option<AuthUser>, AuthError>> {
        Box::pin(async move {
            let Some(cookie_header) = parts.headers.get(COOKIE) else {
                return Ok(None);
            };

            let Ok(cookie_str) = cookie_header.to_str() else {
                return Err(AuthError::InvalidCredentials(
                    "malformed cookie header".into(),
                ));
            };

            // Parse cookies: name=val; name2=val2
            for cookie in cookie_str.split(';') {
                if let Some((name, val)) = cookie.trim().split_once('=')
                    && name == self.cookie_name
                {
                    let session_opt = self.store.load(val).await?;
                    if let Some(session) = session_opt {
                        let mut user = AuthUser::new(session.user_id)
                            .with_roles(session.roles)
                            .with_permissions(session.permissions);
                        user.metadata = session.data;
                        return Ok(Some(user));
                    } else {
                        return Err(AuthError::Expired);
                    }
                }
            }

            Ok(None)
        })
    }
}
