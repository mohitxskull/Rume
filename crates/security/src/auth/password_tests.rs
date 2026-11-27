use super::*;

#[test]
fn argon2id_hash_and_verify_roundtrip_and_rejects_wrong_password() {
    let hasher = PasswordHasher::new(PasswordHasherConfig::low_cost_for_tests());
    let password = SecretString::from("correct-horse-battery-staple");
    let wrong = SecretString::from("wrong-password");

    let phc = hasher
        .hash_password(&password)
        .expect("hashing should succeed");
    assert!(phc.starts_with("$argon2id$v=19$"));
    assert!(
        hasher
            .verify_password(&password, &phc)
            .expect("verify should succeed")
    );
    assert!(
        !hasher
            .verify_password(&wrong, &phc)
            .expect("verify wrong password should return Ok(false)")
    );
    // Non-Argon2id PHC strings must be rejected.
    let non_argon2id = phc.replacen("$argon2id$", "$argon2i$", 1);
    assert!(hasher.verify_password(&password, &non_argon2id).is_err());

    // Debug must redact pepper.
    let mut cfg = PasswordHasherConfig::low_cost_for_tests();
    cfg.pepper = Some(SecretString::from("super-secret-pepper"));
    let dbg = format!("{cfg:?}");
    assert!(!dbg.contains("super-secret-pepper"));
    assert!(dbg.contains("[REDACTED]"));
}

#[test]
fn argon2id_uses_unique_salts_and_detects_needs_rehash_on_param_upgrade() {
    let low = PasswordHasher::new(PasswordHasherConfig::low_cost_for_tests());
    let password = SecretString::from("same-password");

    let h1 = low.hash_password(&password).expect("hash 1");
    let h2 = low.hash_password(&password).expect("hash 2");
    assert_ne!(h1, h2, "unique salts must produce distinct PHC hashes");

    assert!(!low.needs_rehash(&h1));

    let upgraded = PasswordHasher::new(PasswordHasherConfig {
        m_cost: 2048,
        t_cost: 1,
        p_cost: 1,
        output_len: 32,
        pepper: None,
    });
    assert!(upgraded.needs_rehash(&h1));
    assert!(upgraded.needs_rehash("not-a-valid-phc-string"));
}

#[tokio::test]
async fn argon2id_async_helpers_execute_on_blocking_pool_and_preserve_pepper() {
    let mut cfg = PasswordHasherConfig::low_cost_for_tests();
    cfg.pepper = Some(SecretString::from("app-wide-pepper"));
    let pep_hasher = PasswordHasher::new(cfg);

    let no_pep_hasher = PasswordHasher::new(PasswordHasherConfig::low_cost_for_tests());
    let password = SecretString::from("async-secret-pw");

    let phc = pep_hasher
        .hash_password_async(password.clone())
        .await
        .expect("async hash");
    assert!(
        pep_hasher
            .verify_password_async(password.clone(), phc.clone())
            .await
            .expect("async verify with pepper")
    );
    assert!(
        !no_pep_hasher
            .verify_password_async(password, phc)
            .await
            .expect("async verify without pepper returns false")
    );
}
