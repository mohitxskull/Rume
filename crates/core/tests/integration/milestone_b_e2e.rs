//! Milestone B end-to-end integration tests through the `rume` facade.
//!
//! Exercises:
//! 1. JWT/JWKS RS256 Bearer authentication and `kid` rotation over HTTP
//! 2. `SqlApiKeyStore` + `SqlRoleStore` + `DynamicPolicyLayer` live permission revocation over HTTP
//! 3. Argon2id password verification + `SqlSessionStore` cookie login, rotation, and logout over HTTP
//! 4. `PathPrefixTenantResolver` + `CachedDbTenantResolver` + `TenantMembershipLayer` isolation
//! 5. RSA-SHA256 XML-DSig SAML 2.0 assertion verification and async replay protection

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::Json;
use axum::body::Body;
use axum::extract::State;
use axum::http::header::{AUTHORIZATION, COOKIE, SET_COOKIE};
use axum::http::{Request, StatusCode};
use axum::routing::{get, post};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rsa::PrivateKey as RsaPrivateKey;
use rsa::pkcs1v15::SigningKey;
use rsa::signature::RandomizedSigner as _;
use rsa::traits::PublicKeyParts as _;
use rume::auth::{
    ApiKeyScheme, ApiKeyStore, AuthLayer, AuthUser, Authenticated, BearerTokenScheme,
    DynamicPolicyLayer, Jwk, JwkSet, JwksFetcher, JwtAlgorithm, JwtClaimsMapper,
    JwtValidationConfig, JwtVerifier, PasswordHasher, PasswordHasherConfig,
    RequireDynamicPermission, Role, RoleStore, Session, SessionScheme, SessionStore,
    SqlApiKeyStore, SqlRoleStore, SqlSessionStore,
};
use rume::cache::InMemoryCacheBackend;
use rume::error::AppError;
use rume::persistence::{BoxFuture, Migrator, RUME_AUTH_MIGRATION_ID, SqliteBackend};
use rume::prelude::*;
use rume::tenant::{
    CachedDbTenantResolver, HeaderTenantResolver, PathPrefixTenantResolver, SqlTenantStore, Tenant,
    TenantContext, TenantKeyExtractor, TenantLayer, TenantMembershipLayer, TenantStore,
};
use secrecy::{ExposeSecret, SecretString};
use sha2::Sha256;

fn now_epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock after unix epoch")
        .as_secs()
}

async fn setup_auth_sqlite() -> Arc<SqliteBackend> {
    let backend = Arc::new(
        SqliteBackend::in_memory()
            .await
            .expect("create in-memory sqlite backend"),
    );
    let migrator = Migrator::new().with_rume_auth("sqlite");
    let summary = migrator
        .run_pending(backend.as_ref())
        .await
        .expect("run rume_auth sqlite migration");
    assert!(
        summary
            .applied
            .iter()
            .any(|id| id == RUME_AUTH_MIGRATION_ID),
        "expected {RUME_AUTH_MIGRATION_ID} to be applied, got {:?}",
        summary.applied
    );
    backend
}

fn generate_rs256_jwk_and_key(kid: &str) -> (Jwk, SigningKey<Sha256>) {
    let mut rng = rand::rng();
    let priv_key = RsaPrivateKey::new(&mut rng, 2048).expect("generate 2048-bit RSA key");
    let pub_key = priv_key.to_public_key();
    let jwk = Jwk {
        kty: "RSA".into(),
        alg: Some("RS256".into()),
        kid: Some(kid.into()),
        key_use: Some("sig".into()),
        key_ops: Some(vec!["verify".into()]),
        n: Some(URL_SAFE_NO_PAD.encode(pub_key.n().as_ref().to_be_bytes())),
        e: Some(URL_SAFE_NO_PAD.encode(pub_key.e().to_be_bytes())),
        crv: None,
        x: None,
        y: None,
        k: None,
    };
    (jwk, SigningKey::<Sha256>::new(priv_key))
}

fn sign_rs256_jwt(key: &SigningKey<Sha256>, kid: &str, claims: &serde_json::Value) -> String {
    let header = serde_json::json!({ "alg": "RS256", "typ": "JWT", "kid": kid });
    let header_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).unwrap());
    let claims_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_vec(claims).unwrap());
    let signing_input = format!("{header_b64}.{claims_b64}");
    let mut rng = rand::rng();
    let sig = key.sign_with_rng(&mut rng, signing_input.as_bytes());
    let sig_bytes: Box<[u8]> = sig.into();
    let sig_b64 = URL_SAFE_NO_PAD.encode(&sig_bytes);
    format!("{signing_input}.{sig_b64}")
}

#[tokio::test(start_paused = true)]
async fn e2e_jwt_jwks_bearer_auth_and_kid_rotation_over_http() {
    struct RotatingJwksFetcher {
        calls: AtomicUsize,
        sets: Vec<JwkSet>,
    }

    impl JwksFetcher for RotatingJwksFetcher {
        fn fetch_jwks(&self) -> BoxFuture<'_, Result<JwkSet, rume::auth::AuthError>> {
            Box::pin(async move {
                let idx = self.calls.fetch_add(1, Ordering::SeqCst);
                let set = self
                    .sets
                    .get(idx)
                    .cloned()
                    .unwrap_or_else(|| self.sets.last().unwrap().clone());
                Ok(set)
            })
        }
    }

    let now = now_epoch_secs();
    let (jwk1, key1) = generate_rs256_jwk_and_key("kid-1");
    let (jwk2, key2) = generate_rs256_jwk_and_key("kid-2");

    let fetcher = Arc::new(RotatingJwksFetcher {
        calls: AtomicUsize::new(0),
        sets: vec![JwkSet { keys: vec![jwk1] }, JwkSet { keys: vec![jwk2] }],
    });

    struct SharedFetcher(Arc<RotatingJwksFetcher>);
    impl JwksFetcher for SharedFetcher {
        fn fetch_jwks(&self) -> BoxFuture<'_, Result<rume::auth::JwkSet, rume::auth::AuthError>> {
            self.0.fetch_jwks()
        }
    }

    let cfg = JwtValidationConfig {
        allowed_algorithms: BTreeSet::from([JwtAlgorithm::Rs256]),
        expected_issuers: vec!["https://idp.rume.test".into()],
        expected_audiences: vec!["rume-api".into()],
        ..Default::default()
    };

    let verifier = JwtVerifier::from_jwks_fetcher(
        SharedFetcher(Arc::clone(&fetcher)),
        cfg,
        Duration::from_secs(300),
        Duration::from_secs(30),
    )
    .expect("construct JwtVerifier from JWKS fetcher");

    let scheme = BearerTokenScheme::from_jwt_verifier(verifier, JwtClaimsMapper::default());
    let router = rume::routing::Router::new()
        .route(
            "/me",
            get(|Authenticated(user): Authenticated<AuthUser>| async move {
                Json(serde_json::json!({
                    "id": user.id,
                    "tenant_id": user.tenant_id,
                    "roles": user.roles,
                }))
            }),
        )
        .layer(AuthLayer::required().scheme(scheme));

    let app = TestApp::new(router);

    let claims = serde_json::json!({
        "sub": "user-jwt-1",
        "iss": "https://idp.rume.test",
        "aud": "rume-api",
        "exp": now + 600,
        "tenant_id": "tenant-alpha",
        "roles": ["admin"],
    });

    // 1. Token signed with kid-1 succeeds and triggers initial JWKS fetch.
    let tok1 = sign_rs256_jwt(&key1, "kid-1", &claims);
    let req1 = Request::builder()
        .uri("/me")
        .header(AUTHORIZATION, format!("Bearer {tok1}"))
        .body(Body::empty())
        .unwrap();
    let resp1 = app.request(req1).await;
    assert_eq!(resp1.status(), StatusCode::OK);
    let body1: serde_json::Value = serde_json::from_str(&TestApp::body_text(resp1).await).unwrap();
    assert_eq!(body1["id"], "user-jwt-1");
    assert_eq!(body1["tenant_id"], "tenant-alpha");
    assert_eq!(body1["roles"][0], "admin");
    assert_eq!(fetcher.calls.load(Ordering::SeqCst), 1);

    // 2. Advance past min_refresh_interval (30s) and send token with rotated kid-2.
    tokio::time::advance(Duration::from_secs(31)).await;
    let tok2 = sign_rs256_jwt(&key2, "kid-2", &claims);
    let req2 = Request::builder()
        .uri("/me")
        .header(AUTHORIZATION, format!("Bearer {tok2}"))
        .body(Body::empty())
        .unwrap();
    let resp2 = app.request(req2).await;
    assert_eq!(resp2.status(), StatusCode::OK);
    assert_eq!(fetcher.calls.load(Ordering::SeqCst), 2);

    // 3. Unknown kid -> 401 Unauthorized.
    let tok_unknown = sign_rs256_jwt(&key2, "kid-unknown", &claims);
    let req3 = Request::builder()
        .uri("/me")
        .header(AUTHORIZATION, format!("Bearer {tok_unknown}"))
        .body(Body::empty())
        .unwrap();
    let resp3 = app.request(req3).await;
    assert_eq!(resp3.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn e2e_sql_api_key_and_dynamic_rbac_live_revocation_over_http() {
    let backend = setup_auth_sqlite().await;
    let api_key_store = SqlApiKeyStore::new(Arc::clone(&backend));
    let role_store = Arc::new(SqlRoleStore::new(Arc::clone(&backend)));

    let now = now_epoch_secs();
    let issued = api_key_store
        .create(
            "svc-billing",
            Some("tenant-1"),
            "billing-bot",
            vec!["invoice:write".into()],
            Some(now + 3600),
            now,
        )
        .await
        .expect("create SQL API key");
    let plaintext_key = issued.plaintext.expose_secret().to_string();

    role_store
        .create_role(&Role {
            id: "role-editor".into(),
            tenant_id: Some("tenant-1".into()),
            name: "editor".into(),
            description: "Invoice editor".into(),
            permissions: vec!["invoice:write".into()],
        })
        .await
        .expect("create role");
    role_store
        .assign_role("svc-billing", Some("tenant-1"), "role-editor")
        .await
        .expect("assign role");

    let policy = Arc::new(RequireDynamicPermission::new(
        Arc::clone(&role_store),
        "invoice:write",
    ));
    let tenant_resolver = HeaderTenantResolver::new().add_tenant(Tenant::new("tenant-1", "t1"));

    let router = rume::routing::Router::new()
        .route(
            "/invoices",
            post(|Authenticated(user): Authenticated<AuthUser>| async move {
                Json(serde_json::json!({ "created_by": user.id }))
            }),
        )
        .layer(DynamicPolicyLayer::new(policy, "write", "invoice"))
        .layer(AuthLayer::required().scheme(ApiKeyScheme::new().with_store(api_key_store)))
        .layer(TenantLayer::required(tenant_resolver));

    let app = TestApp::new(router);

    // 1. Before revocation -> 200 OK
    let req_ok = Request::builder()
        .method("POST")
        .uri("/invoices")
        .header("x-api-key", &plaintext_key)
        .header("x-tenant-id", "tenant-1")
        .body(Body::empty())
        .unwrap();
    let resp_ok = app.request(req_ok).await;
    assert_eq!(resp_ok.status(), StatusCode::OK);
    let body_ok: serde_json::Value =
        serde_json::from_str(&TestApp::body_text(resp_ok).await).unwrap();
    assert_eq!(body_ok["created_by"], "svc-billing");

    // 2. Live permission revocation -> 403 Forbidden immediately on next request
    role_store
        .revoke_permission("role-editor", "invoice:write")
        .await
        .expect("revoke permission");

    let req_revoked = Request::builder()
        .method("POST")
        .uri("/invoices")
        .header("x-api-key", &plaintext_key)
        .header("x-tenant-id", "tenant-1")
        .body(Body::empty())
        .unwrap();
    let resp_revoked = app.request(req_revoked).await;
    assert_eq!(resp_revoked.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn e2e_sql_session_login_rotation_and_logout_over_http() {
    #[derive(Clone)]
    struct AppState {
        hasher: Arc<PasswordHasher>,
        stored_hash: String,
        sessions: SqlSessionStore<SqliteBackend>,
    }

    let backend = setup_auth_sqlite().await;
    let sessions = SqlSessionStore::new(Arc::clone(&backend));
    let hasher = Arc::new(PasswordHasher::new(PasswordHasherConfig::default()));
    let stored_hash = hasher
        .hash_password(&SecretString::from("correct-horse-battery-staple"))
        .expect("hash password");

    let state = AppState {
        hasher,
        stored_hash,
        sessions: sessions.clone(),
    };

    let protected = rume::routing::Router::new()
        .route(
            "/me",
            get(|Authenticated(user): Authenticated<AuthUser>| async move {
                Json(serde_json::json!({ "id": user.id }))
            }),
        )
        .route(
            "/rotate",
            post(
                |State(st): State<AppState>, req: Request<Body>| async move {
                    let cookie = req
                        .headers()
                        .get(COOKIE)
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("");
                    let old_id = cookie
                        .split(';')
                        .find_map(|p| p.trim().strip_prefix("rume_session="))
                        .ok_or(AppError::Unauthorized)?;
                    let rotated = st
                        .sessions
                        .regenerate_id(old_id)
                        .await
                        .map_err(|_| AppError::Unauthorized)?;
                    Ok::<_, AppError>((
                        [(
                            SET_COOKIE,
                            format!("rume_session={}; HttpOnly; Path=/", rotated.id),
                        )],
                        StatusCode::OK,
                    ))
                },
            ),
        )
        .route(
            "/logout",
            post(
                |State(st): State<AppState>, req: Request<Body>| async move {
                    let cookie = req
                        .headers()
                        .get(COOKIE)
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("");
                    if let Some(sid) = cookie
                        .split(';')
                        .find_map(|p| p.trim().strip_prefix("rume_session="))
                    {
                        st.sessions
                            .destroy(sid)
                            .await
                            .map_err(|e| AppError::internal(e.to_string()))?;
                    }
                    Ok::<_, AppError>(StatusCode::NO_CONTENT)
                },
            ),
        )
        .layer(AuthLayer::required().scheme(SessionScheme::new(sessions.clone())))
        .with_state(state.clone());

    let public = rume::routing::Router::new()
        .route(
            "/login",
            post(
                |State(st): State<AppState>, Json(payload): Json<serde_json::Value>| async move {
                    let pw = SecretString::from(payload["password"].as_str().unwrap_or(""));
                    let ok = st
                        .hasher
                        .verify_password(&pw, &st.stored_hash)
                        .map_err(|e| AppError::internal(e.to_string()))?;
                    if !ok {
                        return Err(AppError::Unauthorized);
                    }
                    let sid = format!("sess_init_{}", uuid::Uuid::new_v4().simple());
                    let session = Session::new(sid.clone(), "user-alice", 600).with_role("member");
                    st.sessions
                        .save(session)
                        .await
                        .map_err(|e| AppError::internal(e.to_string()))?;
                    Ok::<_, AppError>((
                        [(SET_COOKIE, format!("rume_session={sid}; HttpOnly; Path=/"))],
                        StatusCode::OK,
                    ))
                },
            ),
        )
        .with_state(state);

    let app = TestApp::new(public.merge(protected));

    // 1. Login with wrong password -> 401
    let bad_login = Request::builder()
        .method("POST")
        .uri("/login")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"password":"wrong"}"#))
        .unwrap();
    assert_eq!(
        app.request(bad_login).await.status(),
        StatusCode::UNAUTHORIZED
    );

    // 2. Login with valid password -> 200 + Set-Cookie
    let good_login = Request::builder()
        .method("POST")
        .uri("/login")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"password":"correct-horse-battery-staple"}"#))
        .unwrap();
    let login_resp = app.request(good_login).await;
    assert_eq!(login_resp.status(), StatusCode::OK);
    let init_cookie = login_resp
        .headers()
        .get(SET_COOKIE)
        .expect("Set-Cookie on login")
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();

    // 3. Authenticated /me with initial session cookie -> 200
    let me_req = Request::builder()
        .uri("/me")
        .header(COOKIE, &init_cookie)
        .body(Body::empty())
        .unwrap();
    let me_resp = app.request(me_req).await;
    assert_eq!(me_resp.status(), StatusCode::OK);
    let me_json: serde_json::Value =
        serde_json::from_str(&TestApp::body_text(me_resp).await).unwrap();
    assert_eq!(me_json["id"], "user-alice");

    // 4. Rotate session ID -> old cookie invalidated, new cookie works
    let rot_req = Request::builder()
        .method("POST")
        .uri("/rotate")
        .header(COOKIE, &init_cookie)
        .body(Body::empty())
        .unwrap();
    let rot_resp = app.request(rot_req).await;
    assert_eq!(rot_resp.status(), StatusCode::OK);
    let rot_cookie = rot_resp
        .headers()
        .get(SET_COOKIE)
        .expect("Set-Cookie on rotate")
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    assert_ne!(init_cookie, rot_cookie);

    let old_me_req = Request::builder()
        .uri("/me")
        .header(COOKIE, &init_cookie)
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        app.request(old_me_req).await.status(),
        StatusCode::UNAUTHORIZED
    );

    let new_me_req = Request::builder()
        .uri("/me")
        .header(COOKIE, &rot_cookie)
        .body(Body::empty())
        .unwrap();
    assert_eq!(app.request(new_me_req).await.status(), StatusCode::OK);

    // 5. Logout -> rotated cookie destroyed -> /me returns 401
    let logout_req = Request::builder()
        .method("POST")
        .uri("/logout")
        .header(COOKIE, &rot_cookie)
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        app.request(logout_req).await.status(),
        StatusCode::NO_CONTENT
    );

    let after_logout_req = Request::builder()
        .uri("/me")
        .header(COOKIE, &rot_cookie)
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        app.request(after_logout_req).await.status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn e2e_path_prefix_and_cached_db_tenant_resolution_with_membership_isolation() {
    let backend = setup_auth_sqlite().await;
    let tenant_store = Arc::new(SqlTenantStore::new(Arc::clone(&backend)));
    tenant_store
        .upsert(&Tenant::new("tenant-a", "acme"))
        .await
        .expect("insert tenant-a");
    tenant_store
        .upsert(&Tenant::new("tenant-b", "globex"))
        .await
        .expect("insert tenant-b");

    // 1. Verify PathPrefixTenantResolver directly.
    let path_resolver = PathPrefixTenantResolver::new()
        .with_prefix("/t")
        .with_store(Arc::clone(&tenant_store));
    let path_app = TestApp::new(
        rume::routing::Router::new()
            .route(
                "/t/{tenant}/ping",
                get(|TenantContext(t): TenantContext| async move { t.id }),
            )
            .layer(TenantLayer::required(path_resolver)),
    );
    let ping_resp = path_app.get("/t/acme/ping").await;
    assert_eq!(ping_resp.status(), StatusCode::OK);
    assert_eq!(TestApp::body_text(ping_resp).await, "tenant-a");

    // 2. Verify CachedDbTenantResolver + AuthLayer + TenantMembershipLayer::strict().
    let cache = Arc::new(InMemoryCacheBackend::new());
    let cached_resolver = CachedDbTenantResolver::new(
        Arc::clone(&tenant_store),
        cache,
        TenantKeyExtractor::PathPrefix("/t".into()),
    );
    let bearer = BearerTokenScheme::new(|token| async move {
        if token == "token-tenant-a" {
            Ok(Some(AuthUser::new("user-a").with_tenant_id("tenant-a")))
        } else {
            Ok(None)
        }
    });

    let app =
        TestApp::new(
            rume::routing::Router::new()
                .route(
                    "/t/{tenant}/dashboard",
                    get(
                        |TenantContext(t): TenantContext,
                         Authenticated(u): Authenticated<AuthUser>| async move {
                            Json(serde_json::json!({ "tenant": t.id, "user": u.id }))
                        },
                    ),
                )
                .layer(TenantMembershipLayer::strict())
                .layer(AuthLayer::required().scheme(bearer))
                .layer(TenantLayer::required(cached_resolver)),
        );

    // Same-tenant access -> 200 OK
    let req_same = Request::builder()
        .uri("/t/acme/dashboard")
        .header(AUTHORIZATION, "Bearer token-tenant-a")
        .body(Body::empty())
        .unwrap();
    let resp_same = app.request(req_same).await;
    assert_eq!(resp_same.status(), StatusCode::OK);
    let body_same: serde_json::Value =
        serde_json::from_str(&TestApp::body_text(resp_same).await).unwrap();
    assert_eq!(body_same["tenant"], "tenant-a");
    assert_eq!(body_same["user"], "user-a");

    // Cross-tenant access (tenant-a user requesting /t/globex) -> 403 Forbidden
    let req_cross = Request::builder()
        .uri("/t/globex/dashboard")
        .header(AUTHORIZATION, "Bearer token-tenant-a")
        .body(Body::empty())
        .unwrap();
    let resp_cross = app.request(req_cross).await;
    assert_eq!(resp_cross.status(), StatusCode::FORBIDDEN);

    // Unknown tenant slug -> 404 Not Found
    let req_unknown = Request::builder()
        .uri("/t/unknown/dashboard")
        .header(AUTHORIZATION, "Bearer token-tenant-a")
        .body(Body::empty())
        .unwrap();
    let resp_unknown = app.request(req_unknown).await;
    assert_eq!(resp_unknown.status(), StatusCode::NOT_FOUND);
}

#[cfg(feature = "saml")]
#[tokio::test]
async fn e2e_saml_xmldsig_rsa_sha256_and_async_replay_protection() {
    use rume::enterprise::{
        InMemorySamlReplayCache, SamlError, SamlParser, SamlValidationConfig,
        SyncReplayCacheAdapter, XmlDsigSamlVerifier,
    };

    let dir = tempfile::tempdir().expect("create temp dir for openssl keys");
    let key_path = dir.path().join("idp.key.pem");
    let cert_path = dir.path().join("idp.cert.pem");
    let status = std::process::Command::new("openssl")
        .args([
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-subj",
            "/CN=idp.rume.test",
            "-days",
            "30",
            "-keyout",
        ])
        .arg(&key_path)
        .arg("-out")
        .arg(&cert_path)
        .output()
        .expect("run openssl to generate RSA keypair");
    assert!(
        status.status.success(),
        "openssl failed: {}",
        String::from_utf8_lossy(&status.stderr)
    );
    let key_pem = std::fs::read_to_string(&key_path).expect("read key pem");
    let cert_pem = std::fs::read_to_string(&cert_path).expect("read cert pem");
    let cert_b64: String = cert_pem
        .lines()
        .filter(|l| !l.starts_with("-----"))
        .collect();

    let assertion_id = "_e2e_saml_assert_1";
    let template = format!(
        r##"<samlp:Response xmlns:samlp="urn:oasis:names:tc:SAML:2.0:protocol" xmlns:saml="urn:oasis:names:tc:SAML:2.0:assertion" ID="_resp_e2e" Version="2.0" IssueInstant="1970-01-01T00:05:00Z" Destination="https://sp.rume.test/acs" InResponseTo="_req_e2e">
  <saml:Issuer>https://idp.rume.test</saml:Issuer>
  <saml:Assertion ID="{assertion_id}" Version="2.0" IssueInstant="1970-01-01T00:05:00Z">
    <saml:Issuer>https://idp.rume.test</saml:Issuer>
    <ds:Signature xmlns:ds="http://www.w3.org/2000/09/xmldsig#">
      <ds:SignedInfo>
        <ds:CanonicalizationMethod Algorithm="http://www.w3.org/2001/10/xml-exc-c14n#"/>
        <ds:SignatureMethod Algorithm="http://www.w3.org/2001/04/xmldsig-more#rsa-sha256"/>
        <ds:Reference URI="#{assertion_id}">
          <ds:Transforms>
            <ds:Transform Algorithm="http://www.w3.org/2000/09/xmldsig#enveloped-signature"/>
            <ds:Transform Algorithm="http://www.w3.org/2001/10/xml-exc-c14n#"/>
          </ds:Transforms>
          <ds:DigestMethod Algorithm="http://www.w3.org/2001/04/xmlenc#sha256"/>
          <ds:DigestValue/>
        </ds:Reference>
      </ds:SignedInfo>
      <ds:SignatureValue/>
      <ds:KeyInfo><ds:X509Data><ds:X509Certificate>{cert_b64}</ds:X509Certificate></ds:X509Data></ds:KeyInfo>
    </ds:Signature>
    <saml:Subject>
      <saml:NameID>alice@rume.test</saml:NameID>
      <saml:SubjectConfirmation Method="urn:oasis:names:tc:SAML:2.0:cm:bearer">
        <saml:SubjectConfirmationData NotOnOrAfter="1970-01-01T00:16:40Z" Recipient="https://sp.rume.test/acs" InResponseTo="_req_e2e"/>
      </saml:SubjectConfirmation>
    </saml:Subject>
    <saml:Conditions NotBefore="1970-01-01T00:01:40Z" NotOnOrAfter="1970-01-01T00:16:40Z">
      <saml:AudienceRestriction>
        <saml:Audience>https://sp.rume.test</saml:Audience>
      </saml:AudienceRestriction>
    </saml:Conditions>
    <saml:AuthnStatement AuthnInstant="1970-01-01T00:05:00Z">
      <saml:AuthnContext>
        <saml:AuthnContextClassRef>urn:oasis:names:tc:SAML:2.0:ac:classes:PasswordProtectedTransport</saml:AuthnContextClassRef>
      </saml:AuthnContext>
    </saml:AuthnStatement>
  </saml:Assertion>
</samlp:Response>"##
    );

    let key = bergshamra::keys::loader::load_pem_auto(key_pem.as_bytes(), None)
        .expect("load signing key");
    let mut km = bergshamra::KeysManager::new();
    km.add_key(key);
    let dsig_ctx = bergshamra::DsigContext::new_permissive(km);
    let signed_xml = bergshamra::sign(&dsig_ctx, &template).expect("sign SAML response");

    let verifier =
        XmlDsigSamlVerifier::from_pem_cert(&cert_pem).expect("build XmlDsigSamlVerifier");
    let assertion = SamlParser::parse_signed(&signed_xml, &verifier).expect("verify signed SAML");
    assert!(assertion.is_signature_verified());
    assert_eq!(assertion.subject, "alice@rume.test");

    let cfg = SamlValidationConfig {
        expected_audience: Some("https://sp.rume.test"),
        expected_issuer: Some("https://idp.rume.test"),
        expected_destination: Some("https://sp.rume.test/acs"),
        expected_recipient: Some("https://sp.rume.test/acs"),
        expected_in_response_to: Some("_req_e2e"),
        clock_skew_secs: 10,
        ..Default::default()
    };
    let async_cache =
        SyncReplayCacheAdapter::new(Arc::new(InMemorySamlReplayCache::with_max_clock_skew(30)));

    // First validation succeeds.
    SamlParser::validate_browser_sso_async(&assertion, 500, &cfg, &async_cache)
        .await
        .expect("first SAML browser SSO validation succeeds");

    // Second validation with same assertion ID is rejected as replay.
    let replay_err = SamlParser::validate_browser_sso_async(&assertion, 501, &cfg, &async_cache)
        .await
        .expect_err("replay must be rejected");
    assert!(
        matches!(replay_err, SamlError::AssertionReplayed(ref id) if id == assertion_id),
        "expected AssertionReplayed, got {replay_err:?}"
    );
}
