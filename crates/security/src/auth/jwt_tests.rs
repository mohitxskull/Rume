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
    assert!(JwtVerifier::from_jwk_set(jwk_set.clone(), JwtValidationConfig::default()).is_err());

    let verifier = JwtVerifier::from_jwk_set(jwk_set, config_with(&[JwtAlgorithm::Rs256])).unwrap();

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

#[tokio::test(start_paused = true)]
async fn jwks_ttl_refresh_failure_backs_off_for_min_refresh_interval() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    struct FlakyFetcher {
        calls: Arc<AtomicUsize>,
        fail: Arc<AtomicBool>,
        jwk_set: JwkSet,
    }

    impl JwksFetcher for FlakyFetcher {
        fn fetch_jwks(&self) -> BoxFuture<'_, Result<JwkSet, AuthError>> {
            let calls = Arc::clone(&self.calls);
            let fail = Arc::clone(&self.fail);
            let set = self.jwk_set.clone();
            Box::pin(async move {
                calls.fetch_add(1, Ordering::SeqCst);
                if fail.load(Ordering::SeqCst) {
                    Err(AuthError::ProviderError("IdP unavailable".to_owned()))
                } else {
                    Ok(set)
                }
            })
        }
    }

    let secret = b"super-secret-jwks-backoff-key-01";
    let valid_jwk = Jwk {
        kty: "oct".to_owned(),
        kid: Some("k1".to_owned()),
        key_use: Some("sig".to_owned()),
        key_ops: None,
        alg: Some("HS256".to_owned()),
        n: None,
        e: None,
        crv: None,
        x: None,
        y: None,
        k: Some(URL_SAFE_NO_PAD.encode(secret)),
    };

    let calls = Arc::new(AtomicUsize::new(0));
    let fail = Arc::new(AtomicBool::new(false));
    let fetcher = FlakyFetcher {
        calls: Arc::clone(&calls),
        fail: Arc::clone(&fail),
        jwk_set: JwkSet {
            keys: vec![valid_jwk],
        },
    };

    let mut cfg = JwtValidationConfig::default();
    cfg.allowed_algorithms.insert(JwtAlgorithm::Hs256);
    let verifier = JwtVerifier::from_jwks_fetcher(
        fetcher,
        cfg,
        Duration::from_secs(60),
        Duration::from_secs(30),
    )
    .unwrap();

    let header_b64 = URL_SAFE_NO_PAD.encode(r#"{"alg":"HS256","kid":"k1"}"#);
    let payload_b64 = URL_SAFE_NO_PAD.encode(r#"{"sub":"u1","exp":9999999999}"#);
    let signing_input = format!("{header_b64}.{payload_b64}");
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).unwrap();
    mac.update(signing_input.as_bytes());
    let sig_b64 = URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());
    let token = format!("{signing_input}.{sig_b64}");

    // Initial fetch succeeds (calls = 1).
    verifier.verify(&token, 1_000).await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // Advance past cache_ttl (60s) and make IdP fail.
    tokio::time::advance(Duration::from_secs(61)).await;
    fail.store(true, Ordering::SeqCst);

    // First post-TTL request attempts refresh (calls = 2) and falls back consistently to cached
    // keys rather than failing Request 1 while succeeding Request 2 (`P2-18`).
    verifier.verify(&token, 1_061).await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    // Second request 5s later (within min_refresh_interval = 30s) must NOT hammer IdP (calls still 2).
    tokio::time::advance(Duration::from_secs(5)).await;
    verifier.verify(&token, 1_066).await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    // After min_refresh_interval elapses, refresh is retried (calls = 3).
    tokio::time::advance(Duration::from_secs(26)).await;
    fail.store(false, Ordering::SeqCst);
    verifier.verify(&token, 1_092).await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn jwks_slow_timeout_still_enforces_full_backoff_after_completion() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    tokio::time::pause();

    struct SlowFailFetcher {
        calls: Arc<AtomicUsize>,
        slow_fail: Arc<AtomicBool>,
        jwk_set: JwkSet,
    }

    impl JwksFetcher for SlowFailFetcher {
        fn fetch_jwks(&self) -> BoxFuture<'_, Result<JwkSet, AuthError>> {
            let calls = Arc::clone(&self.calls);
            let slow_fail = Arc::clone(&self.slow_fail);
            let set = self.jwk_set.clone();
            Box::pin(async move {
                calls.fetch_add(1, Ordering::SeqCst);
                if slow_fail.load(Ordering::SeqCst) {
                    // Simulate a 15s network timeout (> min_refresh_interval = 10s).
                    tokio::time::sleep(Duration::from_secs(15)).await;
                    Err(AuthError::ProviderError("IdP timed out".to_owned()))
                } else {
                    Ok(set)
                }
            })
        }
    }

    let secret = b"super-secret-jwks-slow-backoff-1";
    let valid_jwk = Jwk {
        kty: "oct".to_owned(),
        kid: Some("k1".to_owned()),
        key_use: Some("sig".to_owned()),
        key_ops: None,
        alg: Some("HS256".to_owned()),
        n: None,
        e: None,
        crv: None,
        x: None,
        y: None,
        k: Some(URL_SAFE_NO_PAD.encode(secret)),
    };
    let calls = Arc::new(AtomicUsize::new(0));
    let slow_fail = Arc::new(AtomicBool::new(false));
    let fetcher = SlowFailFetcher {
        calls: Arc::clone(&calls),
        slow_fail: Arc::clone(&slow_fail),
        jwk_set: JwkSet {
            keys: vec![valid_jwk],
        },
    };
    let mut cfg = JwtValidationConfig::default();
    cfg.allowed_algorithms.insert(JwtAlgorithm::Hs256);
    let verifier = JwtVerifier::from_jwks_fetcher(
        fetcher,
        cfg,
        Duration::from_secs(60),
        Duration::from_secs(10),
    )
    .unwrap();

    let header_b64 = URL_SAFE_NO_PAD.encode(r#"{"alg":"HS256","kid":"k1"}"#);
    let payload_b64 = URL_SAFE_NO_PAD.encode(r#"{"sub":"u1","exp":9999999999}"#);
    let signing_input = format!("{header_b64}.{payload_b64}");
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).unwrap();
    mac.update(signing_input.as_bytes());
    let sig_b64 = URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());
    let token = format!("{signing_input}.{sig_b64}");

    verifier.verify(&token, 1_000).await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // Expire TTL and trigger a 15s slow timeout failure.
    tokio::time::advance(Duration::from_secs(61)).await;
    slow_fail.store(true, Ordering::SeqCst);
    verifier.verify(&token, 1_061).await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    // Immediately (and 5s after completion, well within min_refresh_interval = 10s),
    // backoff MUST prevent another fetch even though the previous attempt took 15s (`P2-18`).
    tokio::time::advance(Duration::from_secs(5)).await;
    verifier.verify(&token, 1_081).await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn parse_jwk_set_skips_malformed_key_when_valid_key_exists_and_fails_if_none_valid() {
    let malformed = Jwk {
        kty: "RSA".to_owned(),
        kid: Some("bad-rsa".to_owned()),
        key_use: Some("sig".to_owned()),
        key_ops: None,
        alg: Some("RS256".to_owned()),
        n: Some("!!!not-base64!!!".to_owned()),
        e: Some("AQAB".to_owned()),
        crv: None,
        x: None,
        y: None,
        k: None,
    };
    let valid = Jwk {
        kty: "oct".to_owned(),
        kid: Some("good-oct".to_owned()),
        key_use: Some("sig".to_owned()),
        key_ops: None,
        alg: Some("HS256".to_owned()),
        n: None,
        e: None,
        crv: None,
        x: None,
        y: None,
        k: Some(URL_SAFE_NO_PAD.encode(b"valid-secret-key")),
    };

    // Mixed keyset: malformed key skipped, valid key kept.
    let parsed = parse_jwk_set(&JwkSet {
        keys: vec![malformed.clone(), valid],
    })
    .expect("valid key must survive malformed sibling");
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].kid.as_deref(), Some("good-oct"));

    // All-malformed keyset: fails closed when zero valid keys remain.
    let err = parse_jwk_set(&JwkSet {
        keys: vec![malformed],
    })
    .err()
    .expect("all-malformed keyset must fail");
    assert!(matches!(err, AuthError::InvalidCredentials(_)));
}
