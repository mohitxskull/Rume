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

/// Trait for extracting a policy resource from an HTTP request.
pub trait PolicyResource: Sized + 'static {
    fn is_authorized<P: Policy<AuthUser, Self>>(
        policy: &P,
        user: &AuthUser,
        req: &Request<Body>,
    ) -> bool;
}

impl PolicyResource for () {
    fn is_authorized<P: Policy<AuthUser, ()>>(
        policy: &P,
        user: &AuthUser,
        _req: &Request<Body>,
    ) -> bool {
        policy.is_authorized(user, &())
    }
}

impl PolicyResource for axum::http::request::Parts {
    fn is_authorized<P: Policy<AuthUser, axum::http::request::Parts>>(
        policy: &P,
        user: &AuthUser,
        req: &Request<Body>,
    ) -> bool {
        let mut dummy = Request::builder()
            .method(req.method().clone())
            .uri(req.uri().clone())
            .version(req.version());
        if let Some(headers_mut) = dummy.headers_mut() {
            *headers_mut = req.headers().clone();
        }
        if let Some(exts_mut) = dummy.extensions_mut() {
            *exts_mut = req.extensions().clone();
        }
        let (parts, _) = dummy.body(()).expect("valid request parts").into_parts();
        policy.is_authorized(user, &parts)
    }
}

/// Resource wrapper indicating the resource should be extracted from request extensions.
#[derive(Debug, Clone)]
pub struct FromExtension<T>(pub T);

impl<T: Clone + Send + Sync + 'static> PolicyResource for FromExtension<T> {
    fn is_authorized<P: Policy<AuthUser, FromExtension<T>>>(
        policy: &P,
        user: &AuthUser,
        req: &Request<Body>,
    ) -> bool {
        if let Some(resource) = req.extensions().get::<T>() {
            policy.is_authorized(user, &FromExtension(resource.clone()))
        } else {
            false
        }
    }
}

/// Middleware layer that enforces an authorization policy on incoming requests.
#[derive(Clone)]
pub struct PolicyLayer<P, R = ()> {
    policy: P,
    _phantom: std::marker::PhantomData<fn() -> R>,
}

impl<P> PolicyLayer<P, ()> {
    /// Creates a new policy layer for the specified policy.
    pub fn new(policy: P) -> Self {
        Self {
            policy,
            _phantom: std::marker::PhantomData,
        }
    }
}

impl<P, R> PolicyLayer<P, R> {
    /// Creates a new policy layer for a policy requiring resource `R`.
    pub fn with_resource(policy: P) -> Self {
        Self {
            policy,
            _phantom: std::marker::PhantomData,
        }
    }
}

impl<P> PolicyLayer<P, axum::http::request::Parts> {
    /// Creates a new policy layer evaluating request [`axum::http::request::Parts`].
    pub fn with_parts(policy: P) -> Self {
        Self {
            policy,
            _phantom: std::marker::PhantomData,
        }
    }
}

impl<P, T: Clone + Send + Sync + 'static> PolicyLayer<P, FromExtension<T>> {
    /// Creates a new policy layer extracting resource `T` from request extensions.
    pub fn from_extension(policy: P) -> Self {
        Self {
            policy,
            _phantom: std::marker::PhantomData,
        }
    }
}

impl<S, P: Clone, R: PolicyResource> Layer<S> for PolicyLayer<P, R> {
    type Service = PolicyService<S, P, R>;

    fn layer(&self, inner: S) -> Self::Service {
        PolicyService {
            inner,
            policy: self.policy.clone(),
            _phantom: std::marker::PhantomData,
        }
    }
}

/// Tower service for policy enforcement.
#[derive(Clone)]
pub struct PolicyService<S, P, R = ()> {
    inner: S,
    policy: P,
    _phantom: std::marker::PhantomData<fn() -> R>,
}

impl<S, P, R> Service<Request<Body>> for PolicyService<S, P, R>
where
    S: Service<Request<Body>, Response = Response<Body>> + Clone + Send + 'static,
    S::Future: Send + 'static,
    P: Policy<AuthUser, R> + Clone + Send + Sync + 'static,
    R: PolicyResource,
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

            if !R::is_authorized(&policy, user, &req) {
                return Ok(AppError::Forbidden.into_response());
            }

            inner.call(req).await
        })
    }
}
