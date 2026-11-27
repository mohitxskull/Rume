//! Pure-Rust JWT and JWKS verification.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, KeyInit, Mac};
use p256::ecdsa::{Signature as P256Signature, VerifyingKey as P256VerifyingKey};
use p384::ecdsa::{Signature as P384Signature, VerifyingKey as P384VerifyingKey};
use rsa::pkcs1v15::{Signature as RsaSignature, VerifyingKey as RsaVerifyingKey};
use rsa::{BoxedUint, PublicKey as RsaPublicKey};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Sha256, Sha384, Sha512};
use signature::Verifier as _;
use subtle::ConstantTimeEq;
use tokio::sync::RwLock;
use tokio::time::Instant;
use zeroize::Zeroizing;

use crate::auth::error::AuthError;
use crate::auth::identity::AuthUser;
use crate::auth::session::BoxFuture;

/// Maximum RSA modulus size accepted from a JWK (8192 bits).
const MAX_RSA_MODULUS_BYTES: usize = 1024;
/// Maximum JWKS HTTP response size (1 MiB).
#[cfg(feature = "http-client")]
const MAX_JWKS_RESPONSE_BYTES: usize = 1024 * 1024;
#[cfg(feature = "http-client")]
const DEFAULT_JWKS_TIMEOUT: Duration = Duration::from_secs(10);

/// Supported JWT signing algorithms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum JwtAlgorithm {
    /// HMAC-SHA256.
    Hs256,
    /// HMAC-SHA384.
    Hs384,
    /// HMAC-SHA512.
    Hs512,
    /// RSASSA-PKCS1-v1_5 using SHA-256.
    Rs256,
    /// RSASSA-PKCS1-v1_5 using SHA-384.
    Rs384,
    /// RSASSA-PKCS1-v1_5 using SHA-512.
    Rs512,
    /// ECDSA using P-256 and SHA-256.
    Es256,
    /// ECDSA using P-384 and SHA-384.
    Es384,
}

impl JwtAlgorithm {
    fn from_jwt_alg(alg: &str) -> Option<Self> {
        match alg {
            "HS256" => Some(Self::Hs256),
            "HS384" => Some(Self::Hs384),
            "HS512" => Some(Self::Hs512),
            "RS256" => Some(Self::Rs256),
            "RS384" => Some(Self::Rs384),
            "RS512" => Some(Self::Rs512),
            "ES256" => Some(Self::Es256),
            "ES384" => Some(Self::Es384),
            _ => None,
        }
    }

    fn as_jwt_alg(self) -> &'static str {
        match self {
            Self::Hs256 => "HS256",
            Self::Hs384 => "HS384",
            Self::Hs512 => "HS512",
            Self::Rs256 => "RS256",
            Self::Rs384 => "RS384",
            Self::Rs512 => "RS512",
            Self::Es256 => "ES256",
            Self::Es384 => "ES384",
        }
    }

    fn is_hmac(self) -> bool {
        matches!(self, Self::Hs256 | Self::Hs384 | Self::Hs512)
    }
}

/// JSON Web Key (RFC 7517).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Jwk {
    /// Key type (`"RSA"`, `"EC"`, `"oct"`).
    pub kty: String,
    /// Key ID.
    pub kid: Option<String>,
    /// Public key use (`"sig"`, `"enc"`).
    #[serde(rename = "use")]
    pub key_use: Option<String>,
    /// Key operations.
    pub key_ops: Option<Vec<String>>,
    /// Algorithm.
    pub alg: Option<String>,
    /// RSA modulus (base64url).
    pub n: Option<String>,
    /// RSA exponent (base64url).
    pub e: Option<String>,
    /// EC curve (`"P-256"`, `"P-384"`).
    pub crv: Option<String>,
    /// EC x coordinate (base64url).
    pub x: Option<String>,
    /// EC y coordinate (base64url).
    pub y: Option<String>,
    /// Symmetric key material (base64url).
    pub k: Option<String>,
}

/// JSON Web Key Set (RFC 7517).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct JwkSet {
    /// Keys in the set.
    pub keys: Vec<Jwk>,
}

/// Validation policy for [`JwtVerifier`].
#[derive(Debug, Clone)]
pub struct JwtValidationConfig {
    /// Explicitly allow-listed JWT algorithms (must be non-empty).
    pub allowed_algorithms: BTreeSet<JwtAlgorithm>,
    /// Accepted `iss` values (empty disables issuer check).
    pub expected_issuers: Vec<String>,
    /// Accepted `aud` values (empty disables audience check).
    pub expected_audiences: Vec<String>,
    /// Clock skew leeway in seconds (default: `30`).
    pub leeway_secs: u64,
    /// Require `exp` claim (default: `true`).
    pub require_exp: bool,
    /// Require `sub` claim (default: `true`).
    pub require_sub: bool,
}

impl Default for JwtValidationConfig {
    fn default() -> Self {
        Self {
            allowed_algorithms: BTreeSet::new(),
            expected_issuers: Vec::new(),
            expected_audiences: Vec::new(),
            leeway_secs: 30,
            require_exp: true,
            require_sub: true,
        }
    }
}

/// Verified JWT claims.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JwtClaims {
    /// Subject (`sub`).
    pub sub: Option<String>,
    /// Issuer (`iss`).
    pub iss: Option<String>,
    /// Audience (`aud`), normalized to a list.
    pub aud: Vec<String>,
    /// Expiration time (`exp`).
    pub exp: Option<u64>,
    /// Not before (`nbf`).
    pub nbf: Option<u64>,
    /// Issued at (`iat`).
    pub iat: Option<u64>,
    /// JWT ID (`jti`).
    pub jti: Option<String>,
    /// Additional custom claims.
    pub extra: Map<String, Value>,
}

impl JwtClaims {
    /// Maps verified JWT claims into an [`AuthUser`].
    pub fn into_auth_user(self) -> Result<AuthUser, AuthError> {
        let sub = self
            .sub
            .filter(|s| !s.is_empty())
            .ok_or_else(|| AuthError::InvalidCredentials("missing sub claim".to_owned()))?;

        let mut user = AuthUser::new(sub);

        if let Some(tenant_id) = self.extra.get("tenant_id").and_then(Value::as_str)
            && !tenant_id.is_empty()
        {
            user = user.with_tenant_id(tenant_id);
        }

        if let Some(roles) = self.extra.get("roles") {
            match roles {
                Value::Array(arr) => {
                    user = user.with_roles(arr.iter().filter_map(Value::as_str));
                }
                Value::String(role) if !role.is_empty() => {
                    user = user.with_role(role);
                }
                _ => {}
            }
        }

        if let Some(Value::Array(perms)) = self.extra.get("permissions") {
            user = user.with_permissions(perms.iter().filter_map(Value::as_str));
        }

        if let Some(scope) = self.extra.get("scope").and_then(Value::as_str) {
            user = user.with_permissions(scope.split_whitespace().filter(|s| !s.is_empty()));
        }

        Ok(user)
    }
}

/// Async trait for fetching a [`JwkSet`] from a remote or test source.
pub trait JwksFetcher: Send + Sync + 'static {
    /// Fetches the current [`JwkSet`].
    fn fetch_jwks(&self) -> BoxFuture<'_, Result<JwkSet, AuthError>>;
}

#[derive(Clone)]
enum KeyMaterial {
    Hmac(Zeroizing<Vec<u8>>),
    Rsa(RsaPublicKey),
    EcP256(P256VerifyingKey),
    EcP384(P384VerifyingKey),
}

#[derive(Clone)]
struct ParsedKey {
    kid: Option<String>,
    alg: Option<JwtAlgorithm>,
    material: KeyMaterial,
}

impl ParsedKey {
    fn is_compatible_with(&self, alg: JwtAlgorithm) -> bool {
        if let Some(key_alg) = self.alg
            && key_alg != alg
        {
            return false;
        }
        matches!(
            (&self.material, alg),
            (
                KeyMaterial::Hmac(_),
                JwtAlgorithm::Hs256 | JwtAlgorithm::Hs384 | JwtAlgorithm::Hs512,
            ) | (
                KeyMaterial::Rsa(_),
                JwtAlgorithm::Rs256 | JwtAlgorithm::Rs384 | JwtAlgorithm::Rs512,
            ) | (KeyMaterial::EcP256(_), JwtAlgorithm::Es256)
                | (KeyMaterial::EcP384(_), JwtAlgorithm::Es384)
        )
    }

    fn verify(&self, alg: JwtAlgorithm, signing_input: &[u8], signature: &[u8]) -> bool {
        if !self.is_compatible_with(alg) {
            return false;
        }
        match (&self.material, alg) {
            (KeyMaterial::Hmac(secret), JwtAlgorithm::Hs256) => {
                verify_hmac::<Hmac<Sha256>>(secret, signing_input, signature)
            }
            (KeyMaterial::Hmac(secret), JwtAlgorithm::Hs384) => {
                verify_hmac::<Hmac<Sha384>>(secret, signing_input, signature)
            }
            (KeyMaterial::Hmac(secret), JwtAlgorithm::Hs512) => {
                verify_hmac::<Hmac<Sha512>>(secret, signing_input, signature)
            }
            (KeyMaterial::Rsa(pk), JwtAlgorithm::Rs256) => {
                let Ok(sig) = RsaSignature::try_from(signature) else {
                    return false;
                };
                RsaVerifyingKey::<Sha256>::new(pk.clone())
                    .verify(signing_input, &sig)
                    .is_ok()
            }
            (KeyMaterial::Rsa(pk), JwtAlgorithm::Rs384) => {
                let Ok(sig) = RsaSignature::try_from(signature) else {
                    return false;
                };
                RsaVerifyingKey::<Sha384>::new(pk.clone())
                    .verify(signing_input, &sig)
                    .is_ok()
            }
            (KeyMaterial::Rsa(pk), JwtAlgorithm::Rs512) => {
                let Ok(sig) = RsaSignature::try_from(signature) else {
                    return false;
                };
                RsaVerifyingKey::<Sha512>::new(pk.clone())
                    .verify(signing_input, &sig)
                    .is_ok()
            }
            (KeyMaterial::EcP256(vk), JwtAlgorithm::Es256) => {
                let Ok(sig) = P256Signature::from_slice(signature) else {
                    return false;
                };
                vk.verify(signing_input, &sig).is_ok()
            }
            (KeyMaterial::EcP384(vk), JwtAlgorithm::Es384) => {
                let Ok(sig) = P384Signature::from_slice(signature) else {
                    return false;
                };
                vk.verify(signing_input, &sig).is_ok()
            }
            _ => false,
        }
    }
}

fn verify_hmac<M: KeyInit + Mac>(secret: &[u8], signing_input: &[u8], signature: &[u8]) -> bool {
    let Ok(mut mac) = M::new_from_slice(secret) else {
        return false;
    };
    mac.update(signing_input);
    let expected = mac.finalize().into_bytes();
    bool::from(expected.as_slice().ct_eq(signature))
}

fn parse_jwk_set(jwk_set: &JwkSet) -> Result<Vec<ParsedKey>, AuthError> {
    let mut parsed = Vec::new();
    for jwk in &jwk_set.keys {
        // RFC 7517 §4.2: skip non-signing keys (e.g. "enc") while treating `use == None` as eligible.
        if let Some(key_use) = jwk.key_use.as_deref()
            && key_use != "sig"
        {
            continue;
        }
        // RFC 7517 §4.3: skip keys whose `key_ops` explicitly excludes `"verify"`.
        if let Some(ops) = jwk.key_ops.as_ref()
            && !ops.iter().any(|op| op == "verify")
        {
            continue;
        }

        let alg = match jwk.alg.as_deref() {
            Some(alg_str) => match JwtAlgorithm::from_jwt_alg(alg_str) {
                Some(a) => Some(a),
                None => continue,
            },
            None => None,
        };

        let material = match jwk.kty.as_str() {
            "RSA" => {
                let n_b64 = jwk.n.as_deref().ok_or_else(|| {
                    AuthError::InvalidCredentials("RSA JWK missing 'n'".to_owned())
                })?;
                let e_b64 = jwk.e.as_deref().ok_or_else(|| {
                    AuthError::InvalidCredentials("RSA JWK missing 'e'".to_owned())
                })?;
                let n_bytes = URL_SAFE_NO_PAD
                    .decode(n_b64)
                    .map_err(|_| AuthError::InvalidCredentials("invalid RSA JWK 'n'".to_owned()))?;
                let e_bytes = URL_SAFE_NO_PAD
                    .decode(e_b64)
                    .map_err(|_| AuthError::InvalidCredentials("invalid RSA JWK 'e'".to_owned()))?;
                if n_bytes.is_empty() || n_bytes.len() > MAX_RSA_MODULUS_BYTES || e_bytes.is_empty()
                {
                    return Err(AuthError::InvalidCredentials(
                        "invalid RSA JWK modulus or exponent".to_owned(),
                    ));
                }
                let n = BoxedUint::from_be_slice_vartime(&n_bytes);
                let e = BoxedUint::from_be_slice_vartime(&e_bytes);
                let pk = RsaPublicKey::new(n, e).map_err(|_| {
                    AuthError::InvalidCredentials("invalid RSA JWK key material".to_owned())
                })?;
                KeyMaterial::Rsa(pk)
            }
            "EC" => {
                let crv = jwk.crv.as_deref().ok_or_else(|| {
                    AuthError::InvalidCredentials("EC JWK missing 'crv'".to_owned())
                })?;
                let coord_len = match crv {
                    "P-256" => 32,
                    "P-384" => 48,
                    _ => continue,
                };
                let x_b64 = jwk.x.as_deref().ok_or_else(|| {
                    AuthError::InvalidCredentials("EC JWK missing 'x'".to_owned())
                })?;
                let y_b64 = jwk.y.as_deref().ok_or_else(|| {
                    AuthError::InvalidCredentials("EC JWK missing 'y'".to_owned())
                })?;
                let x = URL_SAFE_NO_PAD
                    .decode(x_b64)
                    .map_err(|_| AuthError::InvalidCredentials("invalid EC JWK 'x'".to_owned()))?;
                let y = URL_SAFE_NO_PAD
                    .decode(y_b64)
                    .map_err(|_| AuthError::InvalidCredentials("invalid EC JWK 'y'".to_owned()))?;
                if x.len() != coord_len || y.len() != coord_len {
                    return Err(AuthError::InvalidCredentials(
                        "invalid EC JWK coordinate length".to_owned(),
                    ));
                }
                let mut sec1 = Vec::with_capacity(1 + 2 * coord_len);
                sec1.push(0x04);
                sec1.extend_from_slice(&x);
                sec1.extend_from_slice(&y);
                match crv {
                    "P-256" => {
                        let vk = P256VerifyingKey::from_sec1_bytes(&sec1).map_err(|_| {
                            AuthError::InvalidCredentials("invalid P-256 JWK point".to_owned())
                        })?;
                        KeyMaterial::EcP256(vk)
                    }
                    "P-384" => {
                        let vk = P384VerifyingKey::from_sec1_bytes(&sec1).map_err(|_| {
                            AuthError::InvalidCredentials("invalid P-384 JWK point".to_owned())
                        })?;
                        KeyMaterial::EcP384(vk)
                    }
                    _ => unreachable!(),
                }
            }
            "oct" => {
                let k_b64 = jwk.k.as_deref().ok_or_else(|| {
                    AuthError::InvalidCredentials("oct JWK missing 'k'".to_owned())
                })?;
                let k = URL_SAFE_NO_PAD
                    .decode(k_b64)
                    .map_err(|_| AuthError::InvalidCredentials("invalid oct JWK 'k'".to_owned()))?;
                if k.is_empty() {
                    return Err(AuthError::InvalidCredentials(
                        "oct JWK 'k' cannot be empty".to_owned(),
                    ));
                }
                KeyMaterial::Hmac(Zeroizing::new(k))
            }
            _ => continue,
        };

        parsed.push(ParsedKey {
            kid: jwk.kid.clone(),
            alg,
            material,
        });
    }
    Ok(parsed)
}

#[derive(Default)]
struct JwksCache {
    keys: Vec<ParsedKey>,
    fetched_at: Option<Instant>,
    last_refresh_attempt: Option<Instant>,
}

struct DynamicJwks {
    fetcher: Box<dyn JwksFetcher>,
    cache_ttl: Duration,
    min_refresh_interval: Duration,
    cache: RwLock<JwksCache>,
    refresh_lock: tokio::sync::Mutex<()>,
}

enum VerifierBackend {
    StaticHmac(Zeroizing<Vec<u8>>),
    StaticKeys(Vec<ParsedKey>),
    Dynamic(Arc<DynamicJwks>),
}

/// Verifies JWTs against static keys or a cached [`JwksFetcher`].
pub struct JwtVerifier {
    backend: VerifierBackend,
    config: JwtValidationConfig,
}

impl std::fmt::Debug for JwtVerifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JwtVerifier")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

#[derive(Deserialize)]
struct JwtHeader {
    alg: String,
    kid: Option<String>,
    crit: Option<Value>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum AudienceClaim {
    Single(String),
    Many(Vec<String>),
}

#[derive(Deserialize)]
struct RawJwtClaims {
    sub: Option<String>,
    iss: Option<String>,
    aud: Option<AudienceClaim>,
    exp: Option<u64>,
    nbf: Option<u64>,
    iat: Option<u64>,
    jti: Option<String>,
    #[serde(flatten)]
    extra: Map<String, Value>,
}

impl JwtVerifier {
    fn validate_config(config: &JwtValidationConfig) -> Result<(), AuthError> {
        if config.allowed_algorithms.is_empty() {
            return Err(AuthError::InvalidCredentials(
                "JwtValidationConfig::allowed_algorithms must be non-empty".to_owned(),
            ));
        }
        Ok(())
    }

    /// Creates a verifier using a static HMAC secret.
    pub fn from_hmac_secret(
        secret: impl AsRef<[u8]>,
        config: JwtValidationConfig,
    ) -> Result<Self, AuthError> {
        Self::validate_config(&config)?;
        let secret = secret.as_ref();
        if secret.is_empty() {
            return Err(AuthError::InvalidCredentials(
                "HMAC secret cannot be empty".to_owned(),
            ));
        }
        if !config.allowed_algorithms.iter().all(|a| a.is_hmac()) {
            return Err(AuthError::InvalidCredentials(
                "from_hmac_secret only supports HMAC algorithms".to_owned(),
            ));
        }
        Ok(Self {
            backend: VerifierBackend::StaticHmac(Zeroizing::new(secret.to_vec())),
            config,
        })
    }

    /// Creates a verifier using a static [`JwkSet`].
    pub fn from_jwk_set(jwk_set: JwkSet, config: JwtValidationConfig) -> Result<Self, AuthError> {
        Self::validate_config(&config)?;
        let keys = parse_jwk_set(&jwk_set)?;
        Ok(Self {
            backend: VerifierBackend::StaticKeys(keys),
            config,
        })
    }

    /// Creates a verifier backed by a [`JwksFetcher`] with TTL caching and miss rate-limiting.
    pub fn from_jwks_fetcher(
        fetcher: impl JwksFetcher,
        config: JwtValidationConfig,
        cache_ttl: Duration,
        min_refresh_interval: Duration,
    ) -> Result<Self, AuthError> {
        Self::validate_config(&config)?;
        Ok(Self {
            backend: VerifierBackend::Dynamic(Arc::new(DynamicJwks {
                fetcher: Box::new(fetcher),
                cache_ttl,
                min_refresh_interval,
                cache: RwLock::new(JwksCache::default()),
                refresh_lock: tokio::sync::Mutex::new(()),
            })),
            config,
        })
    }

    /// Verifies a compact JWT string at `now_epoch_sec`.
    pub async fn verify(&self, token: &str, now_epoch_sec: u64) -> Result<JwtClaims, AuthError> {
        let mut parts = token.split('.');
        let (Some(header_b64), Some(payload_b64), Some(sig_b64), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(AuthError::InvalidCredentials("malformed JWT".to_owned()));
        };

        if header_b64.is_empty() || payload_b64.is_empty() {
            return Err(AuthError::InvalidCredentials("malformed JWT".to_owned()));
        }

        let header_bytes = URL_SAFE_NO_PAD
            .decode(header_b64)
            .map_err(|_| AuthError::InvalidCredentials("invalid JWT header".to_owned()))?;
        let header: JwtHeader = serde_json::from_slice(&header_bytes)
            .map_err(|_| AuthError::InvalidCredentials("invalid JWT header JSON".to_owned()))?;

        // RFC 7515 §4.1.11: reject any JWT containing a non-empty `crit` header parameter.
        if let Some(crit) = &header.crit
            && !matches!(crit, Value::Array(arr) if arr.is_empty())
        {
            return Err(AuthError::InvalidCredentials(
                "unsupported critical JWT header".to_owned(),
            ));
        }

        let alg = JwtAlgorithm::from_jwt_alg(&header.alg).ok_or_else(|| {
            AuthError::InvalidCredentials(format!("unsupported JWT algorithm '{}'", header.alg))
        })?;
        if !self.config.allowed_algorithms.contains(&alg) {
            return Err(AuthError::InvalidCredentials(format!(
                "disallowed JWT algorithm '{}'",
                alg.as_jwt_alg()
            )));
        }

        let sig_bytes = URL_SAFE_NO_PAD
            .decode(sig_b64)
            .map_err(|_| AuthError::InvalidCredentials("invalid JWT signature".to_owned()))?;
        let signing_input = format!("{header_b64}.{payload_b64}");

        self.verify_signature(
            alg,
            header.kid.as_deref(),
            signing_input.as_bytes(),
            &sig_bytes,
        )
        .await?;

        let payload_bytes = URL_SAFE_NO_PAD
            .decode(payload_b64)
            .map_err(|_| AuthError::InvalidCredentials("invalid JWT payload".to_owned()))?;
        let raw: RawJwtClaims = serde_json::from_slice(&payload_bytes)
            .map_err(|_| AuthError::InvalidCredentials("invalid JWT claims JSON".to_owned()))?;

        let aud = match raw.aud {
            Some(AudienceClaim::Single(s)) => vec![s],
            Some(AudienceClaim::Many(v)) => v,
            None => Vec::new(),
        };

        let claims = JwtClaims {
            sub: raw.sub,
            iss: raw.iss,
            aud,
            exp: raw.exp,
            nbf: raw.nbf,
            iat: raw.iat,
            jti: raw.jti,
            extra: raw.extra,
        };

        self.validate_claims(&claims, now_epoch_sec)?;
        Ok(claims)
    }

    async fn verify_signature(
        &self,
        alg: JwtAlgorithm,
        kid: Option<&str>,
        signing_input: &[u8],
        sig_bytes: &[u8],
    ) -> Result<(), AuthError> {
        match &self.backend {
            VerifierBackend::StaticHmac(secret) => {
                let key = ParsedKey {
                    kid: None,
                    alg: None,
                    material: KeyMaterial::Hmac(secret.clone()),
                };
                if key.verify(alg, signing_input, sig_bytes) {
                    Ok(())
                } else {
                    Err(AuthError::InvalidCredentials(
                        "invalid JWT signature".to_owned(),
                    ))
                }
            }
            VerifierBackend::StaticKeys(keys) => {
                verify_with_key_slice(keys, alg, kid, signing_input, sig_bytes)
            }
            VerifierBackend::Dynamic(dynamic) => {
                Self::verify_dynamic(dynamic, alg, kid, signing_input, sig_bytes).await
            }
        }
    }

    async fn verify_dynamic(
        dynamic: &DynamicJwks,
        alg: JwtAlgorithm,
        kid: Option<&str>,
        signing_input: &[u8],
        sig_bytes: &[u8],
    ) -> Result<(), AuthError> {
        let now = Instant::now();
        let needs_initial_or_ttl_refresh = {
            let cache = dynamic.cache.read().await;
            cache
                .fetched_at
                .is_none_or(|fetched| now.saturating_duration_since(fetched) >= dynamic.cache_ttl)
        };

        if needs_initial_or_ttl_refresh {
            let _refresh_guard = dynamic.refresh_lock.lock().await;
            let still_needs_refresh = {
                let cache = dynamic.cache.read().await;
                let now_r = Instant::now();
                cache.fetched_at.is_none_or(|fetched| {
                    now_r.saturating_duration_since(fetched) >= dynamic.cache_ttl
                })
            };
            if still_needs_refresh {
                // Why: perform network I/O with NO lock held on `cache` so concurrent
                // verifications for already-cached keys remain non-blocking while a
                // refresh is in flight.
                let set = dynamic.fetcher.fetch_jwks().await?;
                let parsed = parse_jwk_set(&set)?;
                let refreshed_at = Instant::now();
                let mut cache = dynamic.cache.write().await;
                cache.keys = parsed;
                cache.fetched_at = Some(refreshed_at);
                cache.last_refresh_attempt = Some(refreshed_at);
            }
        }

        // Fast path under read lock.
        {
            let cache = dynamic.cache.read().await;
            let has_candidate = has_key_candidate(&cache.keys, alg, kid);
            if has_candidate {
                return verify_with_key_slice(&cache.keys, alg, kid, signing_input, sig_bytes);
            }
        }

        // Absent `kid` never triggers a JWKS refresh; fail closed.
        let Some(kid_str) = kid else {
            return Err(AuthError::InvalidCredentials(
                "no unambiguous JWK for token without kid".to_owned(),
            ));
        };

        // Unknown `kid`: rate-limited forced refresh to prevent cache-miss DoS amplification.
        {
            let _refresh_guard = dynamic.refresh_lock.lock().await;
            let should_fetch = {
                let mut cache = dynamic.cache.write().await;
                if has_key_candidate(&cache.keys, alg, Some(kid_str)) {
                    false
                } else {
                    let now_w = Instant::now();
                    let allowed = cache.last_refresh_attempt.is_none_or(|last| {
                        now_w.saturating_duration_since(last) >= dynamic.min_refresh_interval
                    });
                    if allowed {
                        // Why: record attempt timestamp before releasing `cache` and awaiting
                        // fetch so concurrent or failing misses still respect `min_refresh_interval`
                        // without blocking readers on `cache` during network I/O.
                        cache.last_refresh_attempt = Some(now_w);
                        true
                    } else {
                        false
                    }
                }
            };

            if should_fetch {
                let set = dynamic.fetcher.fetch_jwks().await?;
                let parsed = parse_jwk_set(&set)?;
                let mut cache = dynamic.cache.write().await;
                cache.keys = parsed;
                cache.fetched_at = Some(Instant::now());
            }
        }

        let cache = dynamic.cache.read().await;
        verify_with_key_slice(&cache.keys, alg, Some(kid_str), signing_input, sig_bytes)
    }

    fn validate_claims(&self, claims: &JwtClaims, now_epoch_sec: u64) -> Result<(), AuthError> {
        if self.config.require_exp && claims.exp.is_none() {
            return Err(AuthError::InvalidCredentials(
                "missing exp claim".to_owned(),
            ));
        }
        if let Some(exp) = claims.exp
            && now_epoch_sec >= exp.saturating_add(self.config.leeway_secs)
        {
            return Err(AuthError::Expired);
        }

        if let Some(nbf) = claims.nbf
            && now_epoch_sec.saturating_add(self.config.leeway_secs) < nbf
        {
            return Err(AuthError::InvalidCredentials(
                "token not yet valid".to_owned(),
            ));
        }

        if self.config.require_sub && claims.sub.as_deref().is_none_or(str::is_empty) {
            return Err(AuthError::InvalidCredentials(
                "missing sub claim".to_owned(),
            ));
        }

        if !self.config.expected_issuers.is_empty() {
            let valid_iss = claims
                .iss
                .as_ref()
                .is_some_and(|iss| self.config.expected_issuers.iter().any(|e| e == iss));
            if !valid_iss {
                return Err(AuthError::InvalidCredentials(
                    "invalid JWT issuer".to_owned(),
                ));
            }
        }

        if !self.config.expected_audiences.is_empty() {
            let valid_aud = claims
                .aud
                .iter()
                .any(|a| self.config.expected_audiences.iter().any(|e| e == a));
            if !valid_aud {
                return Err(AuthError::InvalidCredentials(
                    "invalid JWT audience".to_owned(),
                ));
            }
        }

        Ok(())
    }
}

fn has_key_candidate(keys: &[ParsedKey], alg: JwtAlgorithm, kid: Option<&str>) -> bool {
    match kid {
        Some(kid) => keys
            .iter()
            .any(|k| k.kid.as_deref() == Some(kid) && k.is_compatible_with(alg)),
        None => keys.iter().filter(|k| k.is_compatible_with(alg)).count() == 1,
    }
}

fn verify_with_key_slice(
    keys: &[ParsedKey],
    alg: JwtAlgorithm,
    kid: Option<&str>,
    signing_input: &[u8],
    sig_bytes: &[u8],
) -> Result<(), AuthError> {
    match kid {
        Some(kid) => {
            let mut matched = keys
                .iter()
                .filter(|k| k.kid.as_deref() == Some(kid) && k.is_compatible_with(alg))
                .peekable();
            if matched.peek().is_none() {
                return Err(AuthError::InvalidCredentials(format!(
                    "no matching JWK for kid '{kid}'"
                )));
            }
            for key in matched {
                if key.verify(alg, signing_input, sig_bytes) {
                    return Ok(());
                }
            }
            Err(AuthError::InvalidCredentials(
                "invalid JWT signature".to_owned(),
            ))
        }
        None => {
            let mut compat = keys.iter().filter(|k| k.is_compatible_with(alg));
            let Some(only_key) = compat.next() else {
                return Err(AuthError::InvalidCredentials(
                    "no compatible JWK for token without kid".to_owned(),
                ));
            };
            if compat.next().is_some() {
                return Err(AuthError::InvalidCredentials(
                    "ambiguous JWKs for token without kid".to_owned(),
                ));
            }
            if only_key.verify(alg, signing_input, sig_bytes) {
                Ok(())
            } else {
                Err(AuthError::InvalidCredentials(
                    "invalid JWT signature".to_owned(),
                ))
            }
        }
    }
}

/// Reqwest-backed [`JwksFetcher`] (enabled by feature `http-client`).
#[cfg(feature = "http-client")]
#[derive(Debug, Clone)]
pub struct ReqwestJwksFetcher {
    jwks_url: String,
    allow_http: bool,
    timeout: Duration,
    max_bytes: usize,
}

#[cfg(feature = "http-client")]
impl ReqwestJwksFetcher {
    /// Creates a new [`ReqwestJwksFetcher`] enforcing HTTPS by default.
    pub fn new(jwks_url: impl Into<String>) -> Result<Self, AuthError> {
        let jwks_url = jwks_url.into();
        let parsed = url::Url::parse(&jwks_url)
            .map_err(|e| AuthError::InvalidCredentials(format!("invalid JWKS URL: {e}")))?;
        if !matches!(parsed.scheme(), "https" | "http") {
            return Err(AuthError::InvalidCredentials(
                "JWKS URL must use http or https".to_owned(),
            ));
        }
        Ok(Self {
            jwks_url,
            allow_http: false,
            timeout: DEFAULT_JWKS_TIMEOUT,
            max_bytes: MAX_JWKS_RESPONSE_BYTES,
        })
    }

    /// Allows plain HTTP URLs for local tests only.
    pub fn allow_http_for_tests(mut self) -> Self {
        self.allow_http = true;
        self
    }
}

#[cfg(feature = "http-client")]
impl JwksFetcher for ReqwestJwksFetcher {
    fn fetch_jwks(&self) -> BoxFuture<'_, Result<JwkSet, AuthError>> {
        Box::pin(async move {
            let parsed = url::Url::parse(&self.jwks_url)
                .map_err(|e| AuthError::InvalidCredentials(format!("invalid JWKS URL: {e}")))?;
            crate::auth::ssrf::validate_outbound_url(&parsed, self.allow_http, "JWKS URL")?;

            let client = crate::auth::ssrf::build_hardened_client(self.timeout, self.allow_http)?;

            let resp = client
                .get(parsed)
                .send()
                .await
                .map_err(|e| AuthError::ProviderError(format!("JWKS fetch failed: {e}")))?;
            if resp.status().is_redirection() {
                return Err(AuthError::ProviderError(format!(
                    "JWKS redirect rejected: HTTP {}",
                    resp.status().as_u16()
                )));
            }
            let mut resp = resp
                .error_for_status()
                .map_err(|e| AuthError::ProviderError(format!("JWKS HTTP error: {e}")))?;

            if let Some(len) = resp.content_length()
                && len > self.max_bytes as u64
            {
                return Err(AuthError::ProviderError(
                    "JWKS response exceeds maximum size".to_owned(),
                ));
            }

            let mut body = Vec::new();
            while let Some(chunk) = resp
                .chunk()
                .await
                .map_err(|e| AuthError::ProviderError(format!("JWKS read failed: {e}")))?
            {
                if body.len().saturating_add(chunk.len()) > self.max_bytes {
                    return Err(AuthError::ProviderError(
                        "JWKS response exceeds maximum size".to_owned(),
                    ));
                }
                body.extend_from_slice(&chunk);
            }

            serde_json::from_slice::<JwkSet>(&body)
                .map_err(|e| AuthError::ProviderError(format!("invalid JWKS JSON: {e}")))
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use hmac::{Hmac, KeyInit, Mac};
    use p256::ecdsa::SigningKey as P256SigningKey;
    use rsa::PrivateKey as RsaPrivateKey;
    use rsa::pkcs1v15::SigningKey as RsaSigningKey;
    use rsa::traits::PublicKeyParts;
    use serde_json::json;
    use sha2::Sha256;
    use signature::{RandomizedSigner, Signer};

    use super::*;

    fn b64(bytes: &[u8]) -> String {
        URL_SAFE_NO_PAD.encode(bytes)
    }

    fn sign_hs256(secret: &[u8], header: &Value, claims: &Value) -> String {
        let h = b64(&serde_json::to_vec(header).unwrap());
        let p = b64(&serde_json::to_vec(claims).unwrap());
        let input = format!("{h}.{p}");
        let mut mac = Hmac::<Sha256>::new_from_slice(secret).unwrap();
        mac.update(input.as_bytes());
        format!("{input}.{}", b64(&mac.finalize().into_bytes()))
    }

    fn generate_rsa_jwk_and_signer(kid: &str) -> (Jwk, RsaSigningKey<Sha256>) {
        let mut rng = rand::rng();
        let priv_key = RsaPrivateKey::new(&mut rng, 2048).unwrap();
        let pub_key = priv_key.to_public_key();
        let jwk = Jwk {
            kty: "RSA".to_owned(),
            kid: Some(kid.to_owned()),
            key_use: Some("sig".to_owned()),
            key_ops: Some(vec!["verify".to_owned()]),
            alg: Some("RS256".to_owned()),
            n: Some(b64(&pub_key.n().as_ref().to_be_bytes())),
            e: Some(b64(&pub_key.e().to_be_bytes())),
            crv: None,
            x: None,
            y: None,
            k: None,
        };
        (jwk, RsaSigningKey::<Sha256>::new(priv_key))
    }

    fn sign_rs256(signer: &RsaSigningKey<Sha256>, header: &Value, claims: &Value) -> String {
        let h = b64(&serde_json::to_vec(header).unwrap());
        let p = b64(&serde_json::to_vec(claims).unwrap());
        let input = format!("{h}.{p}");
        let mut rng = rand::rng();
        let sig = signer.sign_with_rng(&mut rng, input.as_bytes());
        let sig_bytes: Box<[u8]> = sig.into();
        format!("{input}.{}", b64(&sig_bytes))
    }

    fn generate_es256_jwk_and_signer(kid: &str) -> (Jwk, P256SigningKey) {
        let mut sk_bytes = [0u8; 32];
        rand::Rng::fill_bytes(&mut rand::rng(), &mut sk_bytes);
        let signing_key = P256SigningKey::from_bytes(&sk_bytes.into()).unwrap();
        let verifying_key = signing_key.verifying_key();
        let sec1 = verifying_key.to_sec1_point(false);
        let sec1_bytes = sec1.as_bytes();
        // Uncompressed SEC1 point is 0x04 || X (32B) || Y (32B).
        assert_eq!(sec1_bytes.len(), 65);
        let jwk = Jwk {
            kty: "EC".to_owned(),
            kid: Some(kid.to_owned()),
            key_use: Some("sig".to_owned()),
            key_ops: None,
            alg: Some("ES256".to_owned()),
            n: None,
            e: None,
            crv: Some("P-256".to_owned()),
            x: Some(b64(&sec1_bytes[1..33])),
            y: Some(b64(&sec1_bytes[33..65])),
            k: None,
        };
        (jwk, signing_key)
    }

    fn sign_es256(signer: &P256SigningKey, header: &Value, claims: &Value) -> String {
        let h = b64(&serde_json::to_vec(header).unwrap());
        let p = b64(&serde_json::to_vec(claims).unwrap());
        let input = format!("{h}.{p}");
        let sig: p256::ecdsa::Signature = signer.sign(input.as_bytes());
        format!("{input}.{}", b64(&sig.to_bytes()))
    }

    fn config_with(algs: &[JwtAlgorithm]) -> JwtValidationConfig {
        JwtValidationConfig {
            allowed_algorithms: algs.iter().copied().collect(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn jwt_verifier_verifies_hs256_rs256_and_es256_tokens_and_rejects_tampered_signature() {
        let now = 1_700_000_000;
        let claims = json!({ "sub": "alice", "exp": now + 300 });

        // HS256
        let secret = b"super-secret-hmac-key-32-bytes!!";
        let hs_verifier =
            JwtVerifier::from_hmac_secret(secret, config_with(&[JwtAlgorithm::Hs256])).unwrap();
        let hs_tok = sign_hs256(secret, &json!({"alg": "HS256", "typ": "JWT"}), &claims);
        let parsed = hs_verifier.verify(&hs_tok, now).await.unwrap();
        assert_eq!(parsed.sub.as_deref(), Some("alice"));

        let mut tampered_hs = hs_tok.clone();
        tampered_hs.pop();
        tampered_hs.push(if hs_tok.ends_with('A') { 'B' } else { 'A' });
        assert!(matches!(
            hs_verifier.verify(&tampered_hs, now).await,
            Err(AuthError::InvalidCredentials(_))
        ));

        // RS256 + ES256 in one JwkSet
        let (rsa_jwk, rsa_signer) = generate_rsa_jwk_and_signer("rsa-1");
        let (ec_jwk, ec_signer) = generate_es256_jwk_and_signer("ec-1");
        let jwk_set = JwkSet {
            keys: vec![rsa_jwk, ec_jwk],
        };
        let asym_verifier = JwtVerifier::from_jwk_set(
            jwk_set,
            config_with(&[JwtAlgorithm::Rs256, JwtAlgorithm::Es256]),
        )
        .unwrap();

        let rs_tok = sign_rs256(
            &rsa_signer,
            &json!({"alg": "RS256", "kid": "rsa-1"}),
            &claims,
        );
        assert_eq!(
            asym_verifier
                .verify(&rs_tok, now)
                .await
                .unwrap()
                .sub
                .as_deref(),
            Some("alice")
        );

        let es_tok = sign_es256(&ec_signer, &json!({"alg": "ES256", "kid": "ec-1"}), &claims);
        assert_eq!(
            asym_verifier
                .verify(&es_tok, now)
                .await
                .unwrap()
                .sub
                .as_deref(),
            Some("alice")
        );

        let mut tampered_es = es_tok.clone();
        tampered_es.pop();
        tampered_es.push(if es_tok.ends_with('A') { 'B' } else { 'A' });
        assert!(matches!(
            asym_verifier.verify(&tampered_es, now).await,
            Err(AuthError::InvalidCredentials(_))
        ));
    }

    #[tokio::test]
    async fn jwt_verifier_rejects_alg_none_and_disallowed_algorithm_confusion() {
        let now = 1_700_000_000;
        let claims = json!({ "sub": "alice", "exp": now + 300 });
        let (rsa_jwk, _) = generate_rsa_jwk_and_signer("rsa-1");
        let jwk_set = JwkSet {
            keys: vec![rsa_jwk.clone()],
        };

        // Empty allowed_algorithms must be rejected at construction
        assert!(
            JwtVerifier::from_jwk_set(jwk_set.clone(), JwtValidationConfig::default()).is_err()
        );

        let verifier =
            JwtVerifier::from_jwk_set(jwk_set, config_with(&[JwtAlgorithm::Rs256])).unwrap();

        // alg: "none"
        let none_tok = format!(
            "{}.{}.",
            b64(br#"{"alg":"none","typ":"JWT"}"#),
            b64(&serde_json::to_vec(&claims).unwrap())
        );
        assert!(matches!(
            verifier.verify(&none_tok, now).await,
            Err(AuthError::InvalidCredentials(_))
        ));

        // Algorithm confusion: HS256 token signed with RSA modulus bytes against RS256 verifier
        let n_bytes = URL_SAFE_NO_PAD
            .decode(rsa_jwk.n.as_deref().unwrap())
            .unwrap();
        let confused_tok = sign_hs256(&n_bytes, &json!({"alg": "HS256", "kid": "rsa-1"}), &claims);
        assert!(matches!(
            verifier.verify(&confused_tok, now).await,
            Err(AuthError::InvalidCredentials(_))
        ));
    }

    #[tokio::test]
    async fn jwt_verifier_skips_enc_and_unknown_jwks_and_rejects_crit_header() {
        let now = 1_700_000_000;
        let claims = json!({ "sub": "alice", "exp": now + 300 });
        let (rsa_jwk, rsa_signer) = generate_rsa_jwk_and_signer("sig-1");

        let enc_jwk = Jwk {
            kty: "RSA".to_owned(),
            kid: Some("enc-1".to_owned()),
            key_use: Some("enc".to_owned()),
            key_ops: None,
            alg: Some("RSA-OAEP".to_owned()),
            n: Some("AAAA".to_owned()),
            e: Some("AQAB".to_owned()),
            crv: None,
            x: None,
            y: None,
            k: None,
        };
        let okp_jwk = Jwk {
            kty: "OKP".to_owned(),
            kid: Some("okp-1".to_owned()),
            key_use: Some("sig".to_owned()),
            key_ops: None,
            alg: Some("EdDSA".to_owned()),
            n: None,
            e: None,
            crv: Some("Ed25519".to_owned()),
            x: Some("AAAA".to_owned()),
            y: None,
            k: None,
        };

        let verifier = JwtVerifier::from_jwk_set(
            JwkSet {
                keys: vec![enc_jwk, okp_jwk, rsa_jwk],
            },
            config_with(&[JwtAlgorithm::Rs256]),
        )
        .unwrap();

        // Single eligible RS256 key works even without kid
        let valid_no_kid = sign_rs256(&rsa_signer, &json!({"alg": "RS256"}), &claims);
        assert!(verifier.verify(&valid_no_kid, now).await.is_ok());

        // Non-empty crit header must be rejected
        let crit_tok = sign_rs256(
            &rsa_signer,
            &json!({"alg": "RS256", "kid": "sig-1", "crit": ["b64"]}),
            &claims,
        );
        assert!(matches!(
            verifier.verify(&crit_tok, now).await,
            Err(AuthError::InvalidCredentials(_))
        ));
    }

    #[tokio::test]
    async fn jwt_verifier_validates_exp_nbf_leeway_issuer_and_single_or_array_audience() {
        let now = 1_700_000_000;
        let secret = b"0123456789abcdef0123456789abcdef";
        let mut config = config_with(&[JwtAlgorithm::Hs256]);
        config.expected_issuers = vec!["https://idp.example.com".to_owned()];
        config.expected_audiences = vec!["api-a".to_owned()];
        config.leeway_secs = 30;
        let verifier = JwtVerifier::from_hmac_secret(secret, config).unwrap();

        // String aud + within leeway of exp
        let tok_str_aud = sign_hs256(
            secret,
            &json!({"alg": "HS256"}),
            &json!({"sub": "u1", "iss": "https://idp.example.com", "aud": "api-a", "exp": now - 10, "nbf": now + 10}),
        );
        assert!(verifier.verify(&tok_str_aud, now).await.is_ok());

        // Array aud
        let tok_arr_aud = sign_hs256(
            secret,
            &json!({"alg": "HS256"}),
            &json!({"sub": "u1", "iss": "https://idp.example.com", "aud": ["other", "api-a"], "exp": now + 60}),
        );
        let parsed = verifier.verify(&tok_arr_aud, now).await.unwrap();
        assert_eq!(parsed.aud, vec!["other", "api-a"]);

        // Past leeway -> Expired
        let expired_tok = sign_hs256(
            secret,
            &json!({"alg": "HS256"}),
            &json!({"sub": "u1", "iss": "https://idp.example.com", "aud": "api-a", "exp": now - 31}),
        );
        assert!(matches!(
            verifier.verify(&expired_tok, now).await,
            Err(AuthError::Expired)
        ));

        // Wrong issuer -> InvalidCredentials
        let bad_iss = sign_hs256(
            secret,
            &json!({"alg": "HS256"}),
            &json!({"sub": "u1", "iss": "https://evil.example.com", "aud": "api-a", "exp": now + 60}),
        );
        assert!(matches!(
            verifier.verify(&bad_iss, now).await,
            Err(AuthError::InvalidCredentials(_))
        ));

        // Wrong audience -> InvalidCredentials
        let bad_aud = sign_hs256(
            secret,
            &json!({"alg": "HS256"}),
            &json!({"sub": "u1", "iss": "https://idp.example.com", "aud": "other", "exp": now + 60}),
        );
        assert!(matches!(
            verifier.verify(&bad_aud, now).await,
            Err(AuthError::InvalidCredentials(_))
        ));
    }

    struct MockJwksFetcher {
        calls: AtomicUsize,
        sets: Vec<JwkSet>,
    }

    impl JwksFetcher for MockJwksFetcher {
        fn fetch_jwks(&self) -> BoxFuture<'_, Result<JwkSet, AuthError>> {
            let idx = self.calls.fetch_add(1, Ordering::SeqCst);
            let set = self
                .sets
                .get(idx)
                .or_else(|| self.sets.last())
                .unwrap()
                .clone();
            Box::pin(async move { Ok(set) })
        }
    }

    #[tokio::test(start_paused = true)]
    async fn jwt_verifier_refreshes_jwks_on_unknown_kid_and_rate_limits_miss_storms() {
        let now = 1_700_000_000;
        let claims = json!({ "sub": "alice", "exp": now + 300 });
        let (jwk1, signer1) = generate_rsa_jwk_and_signer("kid-1");
        let (jwk2, signer2) = generate_rsa_jwk_and_signer("kid-2");

        let fetcher = Arc::new(MockJwksFetcher {
            calls: AtomicUsize::new(0),
            sets: vec![JwkSet { keys: vec![jwk1] }, JwkSet { keys: vec![jwk2] }],
        });

        struct SharedFetcher(Arc<MockJwksFetcher>);
        impl JwksFetcher for SharedFetcher {
            fn fetch_jwks(&self) -> BoxFuture<'_, Result<JwkSet, AuthError>> {
                self.0.fetch_jwks()
            }
        }

        let verifier = JwtVerifier::from_jwks_fetcher(
            SharedFetcher(Arc::clone(&fetcher)),
            config_with(&[JwtAlgorithm::Rs256]),
            Duration::from_secs(300),
            Duration::from_secs(30),
        )
        .unwrap();

        // 1st call fetches initial JWKS (kid-1)
        let tok1 = sign_rs256(&signer1, &json!({"alg": "RS256", "kid": "kid-1"}), &claims);
        assert!(verifier.verify(&tok1, now).await.is_ok());
        assert_eq!(fetcher.calls.load(Ordering::SeqCst), 1);

        // Advance past min_refresh_interval (30s) -> unknown kid-2 triggers refresh
        tokio::time::advance(Duration::from_secs(31)).await;
        let tok2 = sign_rs256(&signer2, &json!({"alg": "RS256", "kid": "kid-2"}), &claims);
        assert!(verifier.verify(&tok2, now).await.is_ok());
        assert_eq!(fetcher.calls.load(Ordering::SeqCst), 2);

        // Miss storm with bogus kids within 30s cooldown must NOT call fetcher again
        for bogus in ["bogus-1", "bogus-2", "bogus-3"] {
            let tok_bogus = sign_rs256(&signer2, &json!({"alg": "RS256", "kid": bogus}), &claims);
            assert!(matches!(
                verifier.verify(&tok_bogus, now).await,
                Err(AuthError::InvalidCredentials(_))
            ));
        }
        assert_eq!(fetcher.calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn jwt_verifier_cached_kid_succeeds_while_unknown_kid_refresh_is_blocked_on_network() {
        use tokio::sync::Notify;

        struct BlockingJwksFetcher {
            calls: AtomicUsize,
            initial: JwkSet,
            refreshed: JwkSet,
            fetch_started: Arc<Notify>,
            release_fetch: Arc<Notify>,
        }

        impl JwksFetcher for BlockingJwksFetcher {
            fn fetch_jwks(&self) -> BoxFuture<'_, Result<JwkSet, AuthError>> {
                let call = self.calls.fetch_add(1, Ordering::SeqCst);
                let initial = self.initial.clone();
                let refreshed = self.refreshed.clone();
                let started = Arc::clone(&self.fetch_started);
                let release = Arc::clone(&self.release_fetch);
                Box::pin(async move {
                    if call == 0 {
                        Ok(initial)
                    } else {
                        started.notify_one();
                        release.notified().await;
                        Ok(refreshed)
                    }
                })
            }
        }

        let now = 1_700_000_000;
        let claims = json!({ "sub": "alice", "exp": now + 300 });
        let (jwk1, signer1) = generate_rsa_jwk_and_signer("kid-cached");
        let (jwk2, signer2) = generate_rsa_jwk_and_signer("kid-new");

        let fetch_started = Arc::new(Notify::new());
        let release_fetch = Arc::new(Notify::new());
        let fetcher = BlockingJwksFetcher {
            calls: AtomicUsize::new(0),
            initial: JwkSet {
                keys: vec![jwk1.clone()],
            },
            refreshed: JwkSet {
                keys: vec![jwk1, jwk2],
            },
            fetch_started: Arc::clone(&fetch_started),
            release_fetch: Arc::clone(&release_fetch),
        };

        let verifier = Arc::new(
            JwtVerifier::from_jwks_fetcher(
                fetcher,
                config_with(&[JwtAlgorithm::Rs256]),
                Duration::from_secs(300),
                Duration::ZERO,
            )
            .unwrap(),
        );

        let tok_cached = sign_rs256(
            &signer1,
            &json!({"alg": "RS256", "kid": "kid-cached"}),
            &claims,
        );
        let tok_unknown = sign_rs256(
            &signer2,
            &json!({"alg": "RS256", "kid": "kid-new"}),
            &claims,
        );

        // Warm the cache with kid-cached.
        assert!(verifier.verify(&tok_cached, now).await.is_ok());

        // Start a refresh for unknown kid-new; wait until it is blocked on network I/O.
        let v_bg = Arc::clone(&verifier);
        let tok_bg = tok_unknown.clone();
        let bg_task = tokio::spawn(async move { v_bg.verify(&tok_bg, now).await });
        fetch_started.notified().await;

        // While the unknown-kid refresh is blocked on the network, verifying an already-cached
        // kid MUST succeed immediately without blocking on the refresh lock or cache write lock.
        let cached_res = tokio::time::timeout(
            Duration::from_millis(200),
            verifier.verify(&tok_cached, now),
        )
        .await
        .expect("cached kid verification must not block on in-flight JWKS refresh");
        assert!(cached_res.is_ok());

        // Unblock the network fetch and ensure the unknown-kid verification completes cleanly.
        release_fetch.notify_one();
        let new_res = bg_task.await.unwrap();
        assert!(new_res.is_ok());
    }

    #[cfg(feature = "http-client")]
    #[tokio::test]
    async fn reqwest_jwks_fetcher_rejects_ssrf_and_redirects() {
        for blocked in [
            "https://127.0.0.1/.well-known/jwks.json",
            "https://169.254.169.254/latest/meta-data/",
            "https://10.0.0.1/jwks.json",
            "https://[::1]/jwks.json",
            "https://localhost/jwks.json",
        ] {
            let fetcher = ReqwestJwksFetcher::new(blocked).unwrap();
            let err = fetcher
                .fetch_jwks()
                .await
                .expect_err(&format!("expected SSRF rejection for {blocked}"));
            assert!(matches!(err, AuthError::InvalidCredentials(_)));
        }

        let target_hits = Arc::new(AtomicUsize::new(0));
        let target_hits_clone = Arc::clone(&target_hits);
        let app = axum::Router::new()
            .route(
                "/jwks.json",
                axum::routing::get(|| async {
                    (
                        axum::http::StatusCode::FOUND,
                        [(axum::http::header::LOCATION, "/internal")],
                    )
                }),
            )
            .route(
                "/internal",
                axum::routing::get(move || {
                    let hits = Arc::clone(&target_hits_clone);
                    async move {
                        hits.fetch_add(1, Ordering::SeqCst);
                        r#"{"keys":[]}"#
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let fetcher = ReqwestJwksFetcher::new(format!("http://{addr}/jwks.json"))
            .unwrap()
            .allow_http_for_tests();
        let err = fetcher
            .fetch_jwks()
            .await
            .expect_err("expected 302 redirect to be rejected");
        assert!(matches!(err, AuthError::ProviderError(ref msg) if msg.contains("redirect")));
        assert_eq!(target_hits.load(Ordering::SeqCst), 0);
    }
}
