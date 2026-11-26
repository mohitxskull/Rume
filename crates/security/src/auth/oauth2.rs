//! OAuth2 / OIDC authorization-code + PKCE (S256) and state/nonce helpers.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
#[cfg(feature = "http-client")]
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::Rng;
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tokio::sync::RwLock;
use url::Url;

use crate::auth::error::AuthError;
use crate::auth::session::BoxFuture;

#[cfg(feature = "http-client")]
const MAX_DISCOVERY_RESPONSE_BYTES: usize = 1024 * 1024;
#[cfg(feature = "http-client")]
const DEFAULT_DISCOVERY_TIMEOUT: Duration = Duration::from_secs(10);

fn current_epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn random_b64_token() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

/// RFC 7636 PKCE S256 challenge and verifier pair.
#[derive(Clone)]
pub struct PkceChallenge {
    /// Secret code verifier.
    pub code_verifier: SecretString,
    /// Base64url(SHA-256(code_verifier)) challenge.
    pub code_challenge: String,
}

impl std::fmt::Debug for PkceChallenge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PkceChallenge")
            .field("code_verifier", &"[REDACTED]")
            .field("code_challenge", &self.code_challenge)
            .finish()
    }
}

impl PkceChallenge {
    /// Generates a fresh 32-byte CSPRNG PKCE S256 challenge.
    pub fn generate() -> Self {
        let verifier = random_b64_token();
        let digest = Sha256::digest(verifier.as_bytes());
        let code_challenge = URL_SAFE_NO_PAD.encode(digest);
        Self {
            code_verifier: SecretString::from(verifier),
            code_challenge,
        }
    }

    /// Verifies a PKCE `verifier` against `challenge` using S256 in constant time.
    pub fn verify_s256(verifier: &str, challenge: &str) -> bool {
        if verifier.is_empty() || challenge.is_empty() {
            return false;
        }
        let digest = Sha256::digest(verifier.as_bytes());
        let computed = URL_SAFE_NO_PAD.encode(digest);
        bool::from(computed.as_bytes().ct_eq(challenge.as_bytes()))
    }
}

/// OAuth2 / OIDC client configuration.
#[derive(Clone)]
pub struct OAuth2Config {
    /// Client identifier.
    pub client_id: String,
    /// Optional client secret.
    pub client_secret: Option<SecretString>,
    /// Authorization endpoint URL.
    pub authorization_endpoint: String,
    /// Token endpoint URL.
    pub token_endpoint: String,
    /// Redirect URI registered with the provider.
    pub redirect_uri: String,
    /// Requested scopes.
    pub scopes: Vec<String>,
    /// Expected OIDC issuer.
    pub issuer: Option<String>,
    /// Optional JWKS URI.
    pub jwks_uri: Option<String>,
}

impl std::fmt::Debug for OAuth2Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OAuth2Config")
            .field("client_id", &self.client_id)
            .field(
                "client_secret",
                &self.client_secret.as_ref().map(|_| "[REDACTED]"),
            )
            .field("authorization_endpoint", &self.authorization_endpoint)
            .field("token_endpoint", &self.token_endpoint)
            .field("redirect_uri", &self.redirect_uri)
            .field("scopes", &self.scopes)
            .field("issuer", &self.issuer)
            .field("jwks_uri", &self.jwks_uri)
            .finish()
    }
}

/// OIDC Discovery document (`/.well-known/openid-configuration`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OidcDiscovery {
    /// Issuer identifier.
    pub issuer: String,
    /// Authorization endpoint URL.
    pub authorization_endpoint: String,
    /// Token endpoint URL.
    pub token_endpoint: String,
    /// JWKS URI.
    pub jwks_uri: String,
    /// Optional UserInfo endpoint URL.
    pub userinfo_endpoint: Option<String>,
    /// Optional end-session endpoint URL.
    pub end_session_endpoint: Option<String>,
    /// Supported response types.
    #[serde(default)]
    pub response_types_supported: Vec<String>,
    /// Supported PKCE code challenge methods.
    #[serde(default)]
    pub code_challenge_methods_supported: Vec<String>,
}

impl OidcDiscovery {
    /// Parses an OIDC discovery JSON document.
    pub fn from_json(json: &str) -> Result<Self, AuthError> {
        serde_json::from_str(json).map_err(|e| {
            AuthError::InvalidCredentials(format!("invalid OIDC discovery document: {e}"))
        })
    }

    /// Converts this discovery document into [`OAuth2Config`], verifying `expected_issuer`
    /// and enforcing `https://` on all discovered endpoints.
    pub fn to_oauth2_config(
        &self,
        expected_issuer: &str,
        client_id: impl Into<String>,
        client_secret: Option<SecretString>,
        redirect_uri: impl Into<String>,
        scopes: Vec<String>,
    ) -> Result<OAuth2Config, AuthError> {
        self.to_oauth2_config_with_options(
            expected_issuer,
            client_id,
            client_secret,
            redirect_uri,
            scopes,
            false,
        )
    }

    /// Converts this discovery document into [`OAuth2Config`], verifying `expected_issuer`
    /// and validating discovered endpoints (`https://` required unless `allow_http` is `true`).
    pub fn to_oauth2_config_with_options(
        &self,
        expected_issuer: &str,
        client_id: impl Into<String>,
        client_secret: Option<SecretString>,
        redirect_uri: impl Into<String>,
        scopes: Vec<String>,
        allow_http: bool,
    ) -> Result<OAuth2Config, AuthError> {
        if self.issuer != expected_issuer {
            return Err(AuthError::InvalidCredentials(format!(
                "OIDC discovery issuer mismatch: expected '{expected_issuer}', got '{}'",
                self.issuer
            )));
        }

        validate_discovery_endpoint(
            "authorization_endpoint",
            &self.authorization_endpoint,
            allow_http,
        )?;
        validate_discovery_endpoint("token_endpoint", &self.token_endpoint, allow_http)?;
        validate_discovery_endpoint("jwks_uri", &self.jwks_uri, allow_http)?;
        if let Some(ref userinfo) = self.userinfo_endpoint {
            validate_discovery_endpoint("userinfo_endpoint", userinfo, allow_http)?;
        }

        Ok(OAuth2Config {
            client_id: client_id.into(),
            client_secret,
            authorization_endpoint: self.authorization_endpoint.clone(),
            token_endpoint: self.token_endpoint.clone(),
            redirect_uri: redirect_uri.into(),
            scopes,
            issuer: Some(self.issuer.clone()),
            jwks_uri: Some(self.jwks_uri.clone()),
        })
    }
}

fn validate_discovery_endpoint(field: &str, raw: &str, allow_http: bool) -> Result<(), AuthError> {
    let parsed = Url::parse(raw)
        .map_err(|e| AuthError::InvalidCredentials(format!("invalid OIDC {field} URL: {e}")))?;
    if !parsed.has_host() {
        return Err(AuthError::InvalidCredentials(format!(
            "OIDC {field} URL missing host"
        )));
    }
    if allow_http {
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(AuthError::InvalidCredentials(format!(
                "OIDC {field} must use http or https"
            )));
        }
    } else if parsed.scheme() != "https" {
        return Err(AuthError::InvalidCredentials(format!(
            "OIDC {field} must use HTTPS"
        )));
    }
    Ok(())
}

/// Persisted OAuth2 authorization state for callback validation.
#[derive(Clone)]
pub struct OAuth2AuthState {
    /// CSRF state parameter.
    pub state: String,
    /// OIDC nonce parameter.
    pub nonce: String,
    /// PKCE code verifier.
    pub pkce_verifier: SecretString,
    /// Optional post-login redirect target.
    pub redirect_to: Option<String>,
    /// Expiration timestamp (UNIX epoch seconds).
    pub expires_at: u64,
}

impl std::fmt::Debug for OAuth2AuthState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OAuth2AuthState")
            .field("state", &self.state)
            .field("nonce", &"[REDACTED]")
            .field("pkce_verifier", &"[REDACTED]")
            .field("redirect_to", &self.redirect_to)
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

/// Async store for single-use OAuth2 authorization state.
pub trait OAuth2StateStore: Send + Sync + 'static {
    /// Saves an authorization state entry.
    fn save(&self, state: OAuth2AuthState) -> BoxFuture<'_, Result<(), AuthError>>;

    /// Atomically consumes and removes the entry for `state`.
    fn consume<'a>(
        &'a self,
        state: &'a str,
    ) -> BoxFuture<'a, Result<Option<OAuth2AuthState>, AuthError>>;
}

#[derive(Debug, Default)]
struct InMemoryStateMap {
    entries: HashMap<String, OAuth2AuthState>,
    expiry_index: BTreeSet<(u64, String)>,
}

impl InMemoryStateMap {
    fn insert(&mut self, entry: OAuth2AuthState) {
        if let Some(prev) = self.entries.insert(entry.state.clone(), entry.clone()) {
            self.expiry_index.remove(&(prev.expires_at, prev.state));
        }
        self.expiry_index.insert((entry.expires_at, entry.state));
    }

    fn remove(&mut self, state: &str) -> Option<OAuth2AuthState> {
        let removed = self.entries.remove(state)?;
        self.expiry_index
            .remove(&(removed.expires_at, removed.state.clone()));
        Some(removed)
    }

    fn prune_expired(&mut self, now: u64) {
        while let Some((exp, _)) = self.expiry_index.first() {
            if *exp > now {
                break;
            }
            if let Some((_, state)) = self.expiry_index.pop_first()
                && self
                    .entries
                    .get(&state)
                    .is_some_and(|s| s.expires_at <= now)
            {
                self.entries.remove(&state);
            }
        }
    }
}

/// Thread-safe in-memory [`OAuth2StateStore`].
#[derive(Debug, Clone, Default)]
pub struct InMemoryOAuth2StateStore {
    inner: Arc<RwLock<InMemoryStateMap>>,
}

impl InMemoryOAuth2StateStore {
    /// Creates an empty in-memory OAuth2 state store.
    pub fn new() -> Self {
        Self::default()
    }
}

impl OAuth2StateStore for InMemoryOAuth2StateStore {
    fn save(&self, state: OAuth2AuthState) -> BoxFuture<'_, Result<(), AuthError>> {
        Box::pin(async move {
            let now = current_epoch_secs();
            let mut map = self.inner.write().await;
            map.prune_expired(now);
            map.insert(state);
            Ok(())
        })
    }

    fn consume<'a>(
        &'a self,
        state: &'a str,
    ) -> BoxFuture<'a, Result<Option<OAuth2AuthState>, AuthError>> {
        Box::pin(async move {
            let mut map = self.inner.write().await;
            Ok(map.remove(state))
        })
    }
}

/// OAuth2 / OIDC authorization URL builder and callback validator.
#[derive(Debug, Clone)]
pub struct OAuth2Client {
    config: OAuth2Config,
}

impl OAuth2Client {
    /// Creates a new [`OAuth2Client`].
    pub fn new(config: OAuth2Config) -> Self {
        Self { config }
    }

    /// Starts an authorization-code + PKCE flow and persists state.
    pub async fn begin_authorization<S: OAuth2StateStore>(
        &self,
        store: &S,
        ttl_secs: u64,
        now_epoch_sec: u64,
        redirect_to: Option<String>,
    ) -> Result<(Url, OAuth2AuthState), AuthError> {
        let state = random_b64_token();
        let nonce = random_b64_token();
        let pkce = PkceChallenge::generate();

        let auth_state = OAuth2AuthState {
            state: state.clone(),
            nonce: nonce.clone(),
            pkce_verifier: pkce.code_verifier,
            redirect_to,
            expires_at: now_epoch_sec.saturating_add(ttl_secs),
        };

        let mut url = Url::parse(&self.config.authorization_endpoint).map_err(|e| {
            AuthError::InvalidCredentials(format!("invalid authorization_endpoint: {e}"))
        })?;

        {
            let mut pairs = url.query_pairs_mut();
            pairs.append_pair("response_type", "code");
            pairs.append_pair("client_id", &self.config.client_id);
            pairs.append_pair("redirect_uri", &self.config.redirect_uri);
            if !self.config.scopes.is_empty() {
                pairs.append_pair("scope", &self.config.scopes.join(" "));
            }
            pairs.append_pair("state", &state);
            pairs.append_pair("nonce", &nonce);
            pairs.append_pair("code_challenge", &pkce.code_challenge);
            pairs.append_pair("code_challenge_method", "S256");
        }

        store.save(auth_state.clone()).await?;
        Ok((url, auth_state))
    }

    /// Validates a callback `state` and optional ID-token `nonce` in constant time.
    pub async fn validate_callback<S: OAuth2StateStore>(
        &self,
        store: &S,
        received_state: &str,
        id_token_nonce: Option<&str>,
        now_epoch_sec: u64,
    ) -> Result<OAuth2AuthState, AuthError> {
        let Some(auth_state) = store.consume(received_state).await? else {
            return Err(AuthError::InvalidCredentials(
                "unknown or replayed OAuth2 state".to_owned(),
            ));
        };

        if now_epoch_sec >= auth_state.expires_at {
            return Err(AuthError::Expired);
        }

        if let Some(nonce) = id_token_nonce
            && !bool::from(nonce.as_bytes().ct_eq(auth_state.nonce.as_bytes()))
        {
            return Err(AuthError::InvalidCredentials(
                "OAuth2 nonce mismatch".to_owned(),
            ));
        }

        Ok(auth_state)
    }
}

/// Reqwest-backed OIDC discovery client (enabled by feature `http-client`).
#[cfg(feature = "http-client")]
#[derive(Debug, Clone)]
pub struct ReqwestOidcDiscoveryClient {
    allow_http: bool,
}

#[cfg(feature = "http-client")]
impl ReqwestOidcDiscoveryClient {
    /// Creates a new [`ReqwestOidcDiscoveryClient`].
    pub fn new() -> Self {
        Self { allow_http: false }
    }

    /// Allows plain HTTP URLs for local tests only.
    pub fn allow_http_for_tests(mut self) -> Self {
        self.allow_http = true;
        self
    }

    /// Fetches and parses `/.well-known/openid-configuration` for `issuer`.
    pub async fn discover(&self, issuer: &str) -> Result<OidcDiscovery, AuthError> {
        let parsed_issuer = Url::parse(issuer)
            .map_err(|e| AuthError::InvalidCredentials(format!("invalid issuer URL: {e}")))?;
        crate::auth::ssrf::validate_outbound_url(
            &parsed_issuer,
            self.allow_http,
            "OIDC issuer URL",
        )?;

        let discovery_url = format!(
            "{}/.well-known/openid-configuration",
            issuer.trim_end_matches('/')
        );
        let parsed_discovery = Url::parse(&discovery_url).map_err(|e| {
            AuthError::InvalidCredentials(format!("invalid OIDC discovery URL: {e}"))
        })?;
        crate::auth::ssrf::validate_outbound_url(
            &parsed_discovery,
            self.allow_http,
            "OIDC discovery URL",
        )?;

        let client =
            crate::auth::ssrf::build_hardened_client(DEFAULT_DISCOVERY_TIMEOUT, self.allow_http)?;

        let resp =
            client.get(parsed_discovery).send().await.map_err(|e| {
                AuthError::ProviderError(format!("OIDC discovery fetch failed: {e}"))
            })?;
        if resp.status().is_redirection() {
            return Err(AuthError::ProviderError(format!(
                "OIDC discovery redirect rejected: HTTP {}",
                resp.status().as_u16()
            )));
        }
        let mut resp = resp
            .error_for_status()
            .map_err(|e| AuthError::ProviderError(format!("OIDC discovery HTTP error: {e}")))?;

        if let Some(len) = resp.content_length()
            && len > MAX_DISCOVERY_RESPONSE_BYTES as u64
        {
            return Err(AuthError::ProviderError(
                "OIDC discovery response exceeds maximum size".to_owned(),
            ));
        }

        let mut body = Vec::new();
        while let Some(chunk) = resp
            .chunk()
            .await
            .map_err(|e| AuthError::ProviderError(format!("OIDC discovery read failed: {e}")))?
        {
            if body.len().saturating_add(chunk.len()) > MAX_DISCOVERY_RESPONSE_BYTES {
                return Err(AuthError::ProviderError(
                    "OIDC discovery response exceeds maximum size".to_owned(),
                ));
            }
            body.extend_from_slice(&chunk);
        }

        let text = std::str::from_utf8(&body).map_err(|e| {
            AuthError::ProviderError(format!("invalid UTF-8 in OIDC discovery: {e}"))
        })?;
        let doc = OidcDiscovery::from_json(text)?;
        if doc.issuer != issuer {
            return Err(AuthError::InvalidCredentials(format!(
                "OIDC discovery issuer mismatch: expected '{issuer}', got '{}'",
                doc.issuer
            )));
        }
        Ok(doc)
    }
}

#[cfg(feature = "http-client")]
impl Default for ReqwestOidcDiscoveryClient {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_s256_challenge_matches_rfc7636_test_vector_and_verifies_constant_time() {
        // RFC 7636 Appendix B test vector
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let expected_challenge = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
        assert!(PkceChallenge::verify_s256(verifier, expected_challenge));
        assert!(!PkceChallenge::verify_s256(verifier, "wrong-challenge"));
        assert!(!PkceChallenge::verify_s256("", expected_challenge));

        let generated = PkceChallenge::generate();
        use secrecy::ExposeSecret;
        assert!(PkceChallenge::verify_s256(
            generated.code_verifier.expose_secret(),
            &generated.code_challenge
        ));
    }

    #[tokio::test]
    async fn oauth2_begin_authorization_and_single_use_callback_validation_rejects_replay_and_wrong_nonce()
     {
        let store = InMemoryOAuth2StateStore::new();
        let client = OAuth2Client::new(OAuth2Config {
            client_id: "rume-web".to_owned(),
            client_secret: None,
            authorization_endpoint: "https://idp.example.com/oauth2/authorize".to_owned(),
            token_endpoint: "https://idp.example.com/oauth2/token".to_owned(),
            redirect_uri: "https://app.example.com/callback".to_owned(),
            scopes: vec!["openid".to_owned(), "profile".to_owned()],
            issuer: Some("https://idp.example.com".to_owned()),
            jwks_uri: None,
        });

        let now = current_epoch_secs();
        let (url, auth_state) = client
            .begin_authorization(&store, 300, now, Some("/dashboard".to_owned()))
            .await
            .unwrap();

        let query: std::collections::HashMap<_, _> = url.query_pairs().collect();
        assert_eq!(query.get("response_type").map(AsRef::as_ref), Some("code"));
        assert_eq!(query.get("client_id").map(AsRef::as_ref), Some("rume-web"));
        assert_eq!(
            query.get("code_challenge_method").map(AsRef::as_ref),
            Some("S256")
        );
        assert_eq!(
            query.get("state").map(AsRef::as_ref),
            Some(auth_state.state.as_str())
        );
        assert_eq!(
            query.get("nonce").map(AsRef::as_ref),
            Some(auth_state.nonce.as_str())
        );

        // Wrong nonce fails AND consumes state (single-use)
        let (url2, auth_state2) = client
            .begin_authorization(&store, 300, now, None)
            .await
            .unwrap();
        let _ = url2;
        assert!(matches!(
            client
                .validate_callback(&store, &auth_state2.state, Some("wrong-nonce"), now + 10)
                .await,
            Err(AuthError::InvalidCredentials(_))
        ));
        assert!(matches!(
            client
                .validate_callback(
                    &store,
                    &auth_state2.state,
                    Some(&auth_state2.nonce),
                    now + 10
                )
                .await,
            Err(AuthError::InvalidCredentials(_))
        ));

        // Valid callback succeeds once, replay fails
        let validated = client
            .validate_callback(&store, &auth_state.state, Some(&auth_state.nonce), now + 10)
            .await
            .unwrap();
        assert_eq!(validated.redirect_to.as_deref(), Some("/dashboard"));

        assert!(matches!(
            client
                .validate_callback(&store, &auth_state.state, Some(&auth_state.nonce), now + 11)
                .await,
            Err(AuthError::InvalidCredentials(_))
        ));
    }

    #[test]
    fn oidc_discovery_parses_document_and_rejects_issuer_mismatch() {
        let doc_json = r#"{
            "issuer": "https://idp.example.com",
            "authorization_endpoint": "https://idp.example.com/authorize",
            "token_endpoint": "https://idp.example.com/token",
            "jwks_uri": "https://idp.example.com/.well-known/jwks.json",
            "response_types_supported": ["code"],
            "code_challenge_methods_supported": ["S256"]
        }"#;

        let discovery = OidcDiscovery::from_json(doc_json).unwrap();
        let config = discovery
            .to_oauth2_config(
                "https://idp.example.com",
                "client-1",
                None,
                "https://app.example.com/cb",
                vec!["openid".to_owned()],
            )
            .unwrap();
        assert_eq!(config.token_endpoint, "https://idp.example.com/token");
        assert_eq!(
            config.jwks_uri.as_deref(),
            Some("https://idp.example.com/.well-known/jwks.json")
        );

        let mismatch = discovery.to_oauth2_config(
            "https://other-idp.example.com",
            "client-1",
            None,
            "https://app.example.com/cb",
            vec!["openid".to_owned()],
        );
        assert!(matches!(mismatch, Err(AuthError::InvalidCredentials(_))));
    }

    #[test]
    fn oidc_discovery_to_oauth2_config_rejects_non_https_endpoints() {
        let base = OidcDiscovery {
            issuer: "https://idp.example.com".to_owned(),
            authorization_endpoint: "https://idp.example.com/authorize".to_owned(),
            token_endpoint: "https://idp.example.com/token".to_owned(),
            jwks_uri: "https://idp.example.com/.well-known/jwks.json".to_owned(),
            userinfo_endpoint: Some("https://idp.example.com/userinfo".to_owned()),
            end_session_endpoint: None,
            response_types_supported: vec![],
            code_challenge_methods_supported: vec![],
        };

        let mut bad_auth = base.clone();
        bad_auth.authorization_endpoint = "http://idp.example.com/authorize".to_owned();
        assert!(matches!(
            bad_auth.to_oauth2_config(
                "https://idp.example.com",
                "c",
                None,
                "https://app/cb",
                vec![]
            ),
            Err(AuthError::InvalidCredentials(_))
        ));

        let mut bad_token = base.clone();
        bad_token.token_endpoint = "http://idp.example.com/token".to_owned();
        assert!(matches!(
            bad_token.to_oauth2_config(
                "https://idp.example.com",
                "c",
                None,
                "https://app/cb",
                vec![]
            ),
            Err(AuthError::InvalidCredentials(_))
        ));

        let mut bad_jwks = base.clone();
        bad_jwks.jwks_uri = "http://idp.example.com/jwks.json".to_owned();
        assert!(matches!(
            bad_jwks.to_oauth2_config(
                "https://idp.example.com",
                "c",
                None,
                "https://app/cb",
                vec![]
            ),
            Err(AuthError::InvalidCredentials(_))
        ));

        let mut bad_userinfo = base.clone();
        bad_userinfo.userinfo_endpoint = Some("http://idp.example.com/userinfo".to_owned());
        assert!(matches!(
            bad_userinfo.to_oauth2_config(
                "https://idp.example.com",
                "c",
                None,
                "https://app/cb",
                vec![]
            ),
            Err(AuthError::InvalidCredentials(_))
        ));

        // Explicitly allowing HTTP permits HTTP endpoints for local tests.
        assert!(
            bad_userinfo
                .to_oauth2_config_with_options(
                    "https://idp.example.com",
                    "c",
                    None,
                    "https://app/cb",
                    vec![],
                    true
                )
                .is_ok()
        );
    }

    #[cfg(feature = "http-client")]
    #[tokio::test]
    async fn reqwest_oidc_discovery_rejects_ssrf_and_redirects() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let client = ReqwestOidcDiscoveryClient::new();
        for blocked in [
            "https://127.0.0.1",
            "https://169.254.169.254",
            "https://10.0.0.1",
            "https://[::1]",
            "https://localhost",
        ] {
            let err = client
                .discover(blocked)
                .await
                .expect_err(&format!("expected SSRF rejection for {blocked}"));
            assert!(matches!(err, AuthError::InvalidCredentials(_)));
        }

        let target_hits = Arc::new(AtomicUsize::new(0));
        let target_hits_clone = Arc::clone(&target_hits);
        let app = axum::Router::new()
            .route(
                "/.well-known/openid-configuration",
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
                        "{}"
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let test_client = ReqwestOidcDiscoveryClient::new().allow_http_for_tests();
        let err = test_client
            .discover(&format!("http://{addr}"))
            .await
            .expect_err("expected 302 redirect to be rejected");
        assert!(matches!(err, AuthError::ProviderError(ref msg) if msg.contains("redirect")));
        assert_eq!(target_hits.load(Ordering::SeqCst), 0);
    }
}
