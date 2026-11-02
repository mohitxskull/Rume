//! Tower middleware layers for authentication and authorization enforcement.

use std::sync::Arc;
use std::task::{Context, Poll};

use axum::body::Body;
use axum::http::{Request, Response};
use axum::response::IntoResponse;
use tower::{Layer, Service};

use crate::auth::identity::AuthUser;
use crate::auth::policy::Policy;
use crate::auth::schemes::AuthenticationScheme;
use crate::error::AppError;

/// Middleware layer that executes authentication schemes and injects [`AuthUser`] into request extensions.
#[derive(Clone)]
pub struct AuthLayer {
    schemes: Vec<Arc<dyn AuthenticationScheme>>,
    require_auth: bool,
}

impl AuthLayer {
    /// Creates a new authentication layer that optionally authenticates requests.
    pub fn optional() -> Self {
        Self {
            schemes: Vec::new(),
            require_auth: false,
        }
    }

    /// Creates a new authentication layer that rejects unauthenticated requests with 401 Unauthorized.
    pub fn required() -> Self {
        Self {
            schemes: Vec::new(),
            require_auth: true,
        }
    }

    /// Adds an authentication scheme to the pipeline.
    pub fn scheme(mut self, scheme: impl AuthenticationScheme) -> Self {
        self.schemes.push(Arc::new(scheme));
        self
    }
}

impl<S> Layer<S> for AuthLayer {
    type Service = AuthService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        AuthService {
            inner,
            schemes: self.schemes.clone(),
            require_auth: self.require_auth,
        }
    }
}

/// Tower service for request authentication.
#[derive(Clone)]
pub struct AuthService<S> {
    inner: S,
    schemes: Vec<Arc<dyn AuthenticationScheme>>,
    require_auth: bool,
}

impl<S> Service<Request<Body>> for AuthService<S>
where
    S: Service<Request<Body>, Response = Response<Body>> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    type Response = Response<Body>;
    type Error = S::Error;
    type Future = std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>> + Send>,
    >;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<Body>) -> Self::Future {
        let schemes = self.schemes.clone();
        let require_auth = self.require_auth;
        let mut inner = self.inner.clone();

        Box::pin(async move {
            let (parts, body) = req.into_parts();
            let mut identified_user: Option<AuthUser> = None;

            for scheme in schemes {
                match scheme.authenticate(&parts).await {
                    Ok(Some(user)) => {
                        identified_user = Some(user);
                        break;
                    }
                    Ok(None) => continue,
                    Err(err) => {
                        let app_err = AppError::from(err);
                        return Ok(app_err.into_response());
                    }
                }
            }

            if identified_user.is_none() && require_auth {
                return Ok(AppError::Unauthorized.into_response());
            }

            let mut req = Request::from_parts(parts, body);
            if let Some(user) = identified_user {
                req.extensions_mut().insert(user);
            }

            inner.call(req).await
        })
    }
}

/// Middleware layer that enforces an authorization policy on incoming requests.
#[derive(Clone)]
pub struct PolicyLayer<P> {
    policy: P,
}

impl<P> PolicyLayer<P> {
    /// Creates a new policy layer for the specified policy.
    pub fn new(policy: P) -> Self {
        Self { policy }
    }
}

impl<S, P: Clone> Layer<S> for PolicyLayer<P> {
    type Service = PolicyService<S, P>;

    fn layer(&self, inner: S) -> Self::Service {
        PolicyService {
            inner,
            policy: self.policy.clone(),
        }
    }
}

/// Tower service for policy enforcement.
#[derive(Clone)]
pub struct PolicyService<S, P> {
    inner: S,
    policy: P,
}

impl<S, P> Service<Request<Body>> for PolicyService<S, P>
where
    S: Service<Request<Body>, Response = Response<Body>> + Clone + Send + 'static,
    S::Future: Send + 'static,
    P: Policy<AuthUser, ()> + Clone + Send + Sync + 'static,
{
    type Response = Response<Body>;
    type Error = S::Error;
    type Future = std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>> + Send>,
    >;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<Body>) -> Self::Future {
        let policy = self.policy.clone();
        let mut inner = self.inner.clone();

        Box::pin(async move {
            let Some(user) = req.extensions().get::<AuthUser>() else {
                return Ok(AppError::Unauthorized.into_response());
            };

            if !policy.is_authorized(user, &()) {
                return Ok(AppError::Forbidden.into_response());
            }

            inner.call(req).await
        })
    }
}
