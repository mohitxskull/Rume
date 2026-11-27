use std::collections::BTreeSet;

use axum::body::Body;
use axum::http::{Request, Response, StatusCode, header::AUTHORIZATION};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, KeyInit, Mac};
use serde_json::json;
use sha2::Sha256;
use tower::{Layer, ServiceExt};

use super::*;
use crate::auth::identity::AuthUser;
use crate::auth::jwt::{JwtAlgorithm, JwtValidationConfig, JwtVerifier};
use crate::auth::layer::AuthLayer;

fn sign_hs256_jwt(secret: &[u8], claims: &serde_json::Value) -> String {
    let header_b64 = URL_SAFE_NO_PAD.encode(br#"{"alg":"HS256","typ":"JWT"}"#);
    let payload_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_vec(claims).unwrap());
    let signing_input = format!("{header_b64}.{payload_b64}");
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).unwrap();
    mac.update(signing_input.as_bytes());
    let sig_b64 = URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());
    format!("{signing_input}.{sig_b64}")
}

#[tokio::test]
async fn bearer_token_scheme_from_jwt_verifier_populates_auth_user_in_auth_layer() {
    let secret = b"0123456789abcdef0123456789abcdef";
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    let mut allowed = BTreeSet::new();
    allowed.insert(JwtAlgorithm::Hs256);
    let config = JwtValidationConfig {
        allowed_algorithms: allowed,
        expected_issuers: vec!["https://auth.example.com".to_owned()],
        expected_audiences: vec!["rume-api".to_owned()],
        ..Default::default()
    };

    let verifier = JwtVerifier::from_hmac_secret(secret, config).unwrap();
    let scheme = BearerTokenScheme::from_jwt_verifier(verifier, JwtClaimsMapper::default());
    let layer = AuthLayer::required().scheme(scheme);

    let svc = layer.layer(tower::service_fn(|req: Request<Body>| async move {
        let user = req.extensions().get::<AuthUser>().cloned().unwrap();
        let body = format!("{}:{:?}:{}", user.id, user.tenant_id, user.roles.join(","));
        Ok::<_, std::convert::Infallible>(Response::new(Body::from(body)))
    }));

    let token = sign_hs256_jwt(
        secret,
        &json!({
            "sub": "user-42",
            "iss": "https://auth.example.com",
            "aud": "rume-api",
            "exp": now + 300,
            "tenant_id": "tenant-a",
            "roles": ["admin", "editor"],
            "permissions": ["users:read"]
        }),
    );

    let req = Request::builder()
        .header(AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    let resp = svc.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(
        std::str::from_utf8(&bytes).unwrap(),
        "user-42:Some(\"tenant-a\"):admin,editor"
    );
}

#[tokio::test]
async fn api_key_scheme_with_store_authenticates_and_maps_scopes_and_tenant() {
    use crate::auth::api_key::{ApiKeyStore, SqlApiKeyStore};
    use rume_data::persistence::{Migrator, SqliteBackend};
    use secrecy::ExposeSecret;

    let backend = Arc::new(SqliteBackend::in_memory().await.unwrap());
    let migrator = Migrator::new().with_rume_auth("sqlite");
    migrator.run_pending(backend.as_ref()).await.unwrap();

    let store = SqlApiKeyStore::new(backend);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let issued = store
        .create(
            "svc-user",
            Some("tenant-xyz"),
            "service-key",
            vec!["billing:read".into(), "invoices:write".into()],
            Some(now + 3600),
            now,
        )
        .await
        .unwrap();

    let scheme = ApiKeyScheme::new().with_store(store);
    let layer = AuthLayer::required().scheme(scheme);
    let svc = layer.layer(tower::service_fn(|req: Request<Body>| async move {
        let user = req.extensions().get::<AuthUser>().cloned().unwrap();
        let body = format!(
            "{}:{:?}:{}",
            user.id,
            user.tenant_id,
            user.permissions.join(",")
        );
        Ok::<_, std::convert::Infallible>(Response::new(Body::from(body)))
    }));

    let req = Request::builder()
        .header("x-api-key", issued.plaintext.expose_secret())
        .body(Body::empty())
        .unwrap();
    let resp = svc.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(
        std::str::from_utf8(&bytes).unwrap(),
        "svc-user:Some(\"tenant-xyz\"):billing:read,invoices:write"
    );
}

#[tokio::test]
async fn session_scheme_caps_cookie_candidate_store_lookups() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::auth::session::Session;

    struct CountingStore {
        loads: Arc<AtomicUsize>,
    }

    impl SessionStore for CountingStore {
        fn load(&self, _id: &str) -> BoxFuture<'_, Result<Option<Session>, AuthError>> {
            self.loads.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(None) })
        }
        fn save(&self, _session: Session) -> BoxFuture<'_, Result<(), AuthError>> {
            Box::pin(async { Ok(()) })
        }
        fn destroy(&self, _id: &str) -> BoxFuture<'_, Result<(), AuthError>> {
            Box::pin(async { Ok(()) })
        }
        fn regenerate_id<'a>(
            &'a self,
            _old_id: &'a str,
        ) -> BoxFuture<'a, Result<Session, AuthError>> {
            Box::pin(async { Err(AuthError::InvalidCredentials("unused".into())) })
        }
    }

    let loads = Arc::new(AtomicUsize::new(0));
    let scheme = SessionScheme::new(CountingStore {
        loads: Arc::clone(&loads),
    });

    let cookie_header = (0..25)
        .map(|i| format!("rume_session=bogus-{i}"))
        .collect::<Vec<_>>()
        .join("; ");
    let req = Request::builder()
        .header(COOKIE, cookie_header)
        .body(())
        .unwrap();
    let (parts, ()) = req.into_parts();

    let res = scheme.authenticate(&parts).await.unwrap();
    assert!(res.is_none());
    assert_eq!(loads.load(Ordering::SeqCst), MAX_SESSION_COOKIE_CANDIDATES);
}

#[tokio::test]
async fn p2_17_bearer_case_insensitive_and_duplicate_auth_or_api_key_headers_rejected() {
    let bearer = BearerTokenScheme::new(|tok| {
        let ok = tok == "good-tok";
        Box::pin(async move {
            if ok {
                Ok(Some(AuthUser::new("u-bearer")))
            } else {
                Err(AuthError::InvalidCredentials("bad".into()))
            }
        })
    });

    // Lowercase and uppercase `bearer` scheme accepted per RFC 6750 / RFC 9110.
    for hdr in ["bearer good-tok", "BEARER good-tok", "BeArEr good-tok"] {
        let (parts, ()) = Request::builder()
            .header(AUTHORIZATION, hdr)
            .body(())
            .unwrap()
            .into_parts();
        let user = bearer
            .authenticate(&parts)
            .await
            .expect("case-insensitive bearer must authenticate")
            .expect("user present");
        assert_eq!(user.id, "u-bearer");
    }

    // Duplicate Authorization headers rejected.
    let (dup_auth_parts, ()) = Request::builder()
        .header(AUTHORIZATION, "Bearer good-tok")
        .header(AUTHORIZATION, "Bearer other-tok")
        .body(())
        .unwrap()
        .into_parts();
    assert!(matches!(
        bearer.authenticate(&dup_auth_parts).await,
        Err(AuthError::InvalidCredentials(_))
    ));

    // Duplicate X-Api-Key headers rejected.
    let api_key = ApiKeyScheme::new().add_key("k1", AuthUser::new("u-key"));
    let (dup_key_parts, ()) = Request::builder()
        .header("x-api-key", "k1")
        .header("x-api-key", "k2")
        .body(())
        .unwrap()
        .into_parts();
    assert!(matches!(
        api_key.authenticate(&dup_key_parts).await,
        Err(AuthError::InvalidCredentials(_))
    ));
}
