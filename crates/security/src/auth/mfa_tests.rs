use super::*;

#[test]
fn totp_matches_rfc6238_test_vectors_for_sha1_sha256_sha512() {
    let sha1_secret = TotpSecret::from_bytes(
        b"12345678901234567890".to_vec(),
        TotpConfig {
            algorithm: TotpAlgorithm::Sha1,
            digits: 8,
            period_secs: 30,
            skew_steps: 0,
        },
    )
    .unwrap();

    let sha256_secret = TotpSecret::from_bytes(
        b"12345678901234567890123456789012".to_vec(),
        TotpConfig {
            algorithm: TotpAlgorithm::Sha256,
            digits: 8,
            period_secs: 30,
            skew_steps: 0,
        },
    )
    .unwrap();

    let sha512_secret = TotpSecret::from_bytes(
        b"1234567890123456789012345678901234567890123456789012345678901234".to_vec(),
        TotpConfig {
            algorithm: TotpAlgorithm::Sha512,
            digits: 8,
            period_secs: 30,
            skew_steps: 0,
        },
    )
    .unwrap();

    let vectors = [
        (59_u64, "94287082", "46119246", "90693936"),
        (1_111_111_109, "07081804", "68084774", "25091201"),
        (1_111_111_111, "14050471", "67062674", "99943326"),
        (1_234_567_890, "89005924", "91819424", "93441116"),
        (2_000_000_000, "69279037", "90698825", "38618901"),
        (20_000_000_000, "65353130", "77737706", "47863826"),
    ];

    for (ts, exp_sha1, exp_sha256, exp_sha512) in vectors {
        assert_eq!(sha1_secret.generate_code(ts).unwrap(), exp_sha1);
        assert_eq!(sha256_secret.generate_code(ts).unwrap(), exp_sha256);
        assert_eq!(sha512_secret.generate_code(ts).unwrap(), exp_sha512);
    }
}

#[test]
fn totp_verify_accepts_skew_window_and_returns_matched_step_for_replay_prevention() {
    let secret = TotpSecret::generate(TotpConfig::default());
    let step_10_ts = 300_u64;
    let code_at_step_9 = secret.generate_code(270).unwrap();
    let code_at_step_11 = secret.generate_code(330).unwrap();
    let code_at_step_12 = secret.generate_code(360).unwrap();

    assert_eq!(secret.verify_code(&code_at_step_9, step_10_ts), Some(9));
    assert_eq!(secret.verify_code(&code_at_step_11, step_10_ts), Some(11));
    assert_eq!(secret.verify_code(&code_at_step_12, step_10_ts), None);
    assert_eq!(secret.verify_code("000000", step_10_ts), None);
}

#[test]
fn totp_otpauth_uri_escapes_issuer_and_account_and_roundtrips_base32() {
    let secret = TotpSecret::generate(TotpConfig::default());
    let b32 = secret.to_base32();
    let lower_b32 = b32.to_ascii_lowercase();
    let decoded = TotpSecret::from_base32(&lower_b32, TotpConfig::default()).unwrap();
    assert_eq!(decoded.to_base32(), b32);

    let uri = secret.otpauth_uri("Acme Corp", "alice+dev@example.com");
    assert!(uri.starts_with("otpauth://totp/Acme%20Corp:alice%2Bdev%40example.com?"));
    assert!(uri.contains(&format!("secret={b32}")));
    assert!(uri.contains("issuer=Acme%20Corp"));
    assert!(uri.contains("algorithm=SHA1"));
    assert!(uri.contains("digits=6"));
    assert!(uri.contains("period=30"));
}

#[test]
fn recovery_codes_verify_once_and_never_store_plaintext() {
    let (mut set, plaintext) = RecoveryCodeSet::generate(4);
    assert_eq!(plaintext.len(), 4);
    assert_eq!(set.remaining(), 4);

    let first = &plaintext[0];
    assert!(first.contains('-'));
    assert!(set.verify_and_consume(first));
    assert_eq!(set.remaining(), 3);
    // Single-use: second attempt must fail.
    assert!(!set.verify_and_consume(first));
    assert!(!set.verify_and_consume("INVALID-CODE-0000"));
}
