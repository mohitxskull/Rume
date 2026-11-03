//! Authentication and authorization foundations.
//!
//! Provides pluggable authentication schemes (Bearer tokens, API keys, session cookies),
//! identity extractors ([`Authenticated`], [`OptionalAuth`]), session storage, and declarative
//! authorization policies ([`Policy`], [`RequireRole`], [`RequirePermission`]).

pub mod error;
pub mod identity;
pub mod layer;
pub mod policy;
#[cfg(feature = "redis")]
pub mod redis;
pub mod schemes;
pub mod session;

#[cfg(feature = "redis")]
pub use self::redis::RedisSessionStore;
pub use error::AuthError;
pub use identity::{AuthUser, Authenticated, Identity, OptionalAuth};
pub use layer::{AuthLayer, PolicyLayer};
pub use policy::{
    CustomPolicy, Policy, RequireAllPermissions, RequireAnyRole, RequirePermission, RequireRole,
    authorize,
};
pub use schemes::{ApiKeyScheme, AuthenticationScheme, BearerTokenScheme, SessionScheme};
pub use session::{InMemorySessionStore, Session, SessionStore};
