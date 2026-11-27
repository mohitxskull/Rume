//! Authentication and authorization foundations.
//!
//! Provides pluggable authentication schemes (Bearer tokens, API keys, session cookies),
//! identity extractors ([`Authenticated`], [`OptionalAuth`]), session storage, and declarative
//! authorization policies ([`Policy`], [`RequireRole`], [`RequirePermission`]).

pub mod api_key;
pub mod error;
pub mod identity;
pub mod jwt;
pub mod layer;
pub mod mfa;
pub mod oauth2;
pub mod password;
pub mod policy;
pub mod rbac;
#[cfg(feature = "redis")]
pub mod redis;
pub mod schemes;
pub mod session;
pub mod sql_session;
#[cfg(feature = "http-client")]
pub(crate) mod ssrf;
pub mod tokens;

#[cfg(feature = "redis")]
pub use self::redis::RedisSessionStore;
pub use api_key::{ApiKeyRecord, ApiKeyStore, InMemoryApiKeyStore, IssuedApiKey, SqlApiKeyStore};
pub use error::AuthError;
pub use identity::{AuthUser, Authenticated, Identity, OptionalAuth};
#[cfg(feature = "http-client")]
pub use jwt::ReqwestJwksFetcher;
pub use jwt::{
    Jwk, JwkSet, JwksFetcher, JwtAlgorithm, JwtClaims, JwtValidationConfig, JwtVerifier,
};
pub use layer::{AuthLayer, FromExtension, PolicyLayer, PolicyResource};
pub use mfa::{
    RecoveryCodeSet, TotpAlgorithm, TotpConfig, TotpSecret, WebAuthnAssertion, WebAuthnChallenge,
    WebAuthnVerification, WebAuthnVerifier,
};
#[cfg(feature = "http-client")]
pub use oauth2::ReqwestOidcDiscoveryClient;
pub use oauth2::{
    InMemoryOAuth2StateStore, OAuth2AuthState, OAuth2Client, OAuth2Config, OAuth2StateStore,
    OidcDiscovery, PkceChallenge,
};
pub use password::{PasswordHasher, PasswordHasherConfig};
pub use policy::{
    CustomPolicy, Policy, RequireAllPermissions, RequireAnyRole, RequirePermission, RequireRole,
    authorize,
};
pub use rbac::{
    AbacContext, AbacPolicy, AllOfPolicy, AnyOfPolicy, DynamicPolicyLayer, InMemoryRoleStore,
    RequireDynamicPermission, Role, RoleStore, SqlRoleStore,
};
pub use schemes::{
    ApiKeyScheme, AuthenticationScheme, BearerTokenScheme, JwtClaimsMapper, SessionScheme,
};
pub use session::{InMemorySessionStore, Session, SessionStore};
pub use sql_session::SqlSessionStore;
pub use tokens::{SignedTokenClaims, SignedUrl, TokenSigner};
