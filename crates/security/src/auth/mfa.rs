//! Multi-factor authentication: RFC 6238 TOTP, recovery codes, and WebAuthn hook.

use hmac::{Hmac, KeyInit, Mac};
use rand::Rng;
use rume_foundation::util::safe_equal;
use sha1::Sha1;
use sha2::{Digest, Sha256, Sha512};
use zeroize::Zeroizing;

use crate::auth::error::AuthError;
use crate::auth::session::BoxFuture;

const BASE32_ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
const RECOVERY_ALPHABET: &[u8; 32] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";

/// Hash algorithm used by RFC 6238 TOTP.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TotpAlgorithm {
    /// HMAC-SHA1 (default for authenticator app compatibility).
    #[default]
    Sha1,
    /// HMAC-SHA256.
    Sha256,
    /// HMAC-SHA512.
    Sha512,
}

/// Configuration for TOTP generation and verification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TotpConfig {
    /// HMAC algorithm.
    pub algorithm: TotpAlgorithm,
    /// Number of digits (`6..=8`, default `6`).
    pub digits: u32,
    /// Time step period in seconds (default `30`).
    pub period_secs: u64,
    /// Allowed clock drift in steps before/after current step (default `1`).
    // Why: +/- 1 step (30s) tolerates typical client/server clock skew without
    // overly widening the replay window.
    pub skew_steps: u64,
}

impl Default for TotpConfig {
    fn default() -> Self {
        Self {
            algorithm: TotpAlgorithm::Sha1,
            digits: 6,
            period_secs: 30,
            skew_steps: 1,
        }
    }
}

/// Secret key material for RFC 6238 TOTP.
#[derive(Clone)]
pub struct TotpSecret {
    bytes: Zeroizing<Vec<u8>>,
    config: TotpConfig,
}

impl std::fmt::Debug for TotpSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TotpSecret")
            .field("bytes", &"[REDACTED]")
            .field("config", &self.config)
            .finish()
    }
}

impl TotpSecret {
    fn validate_config(config: &TotpConfig) -> Result<(), AuthError> {
        if !(6..=8).contains(&config.digits) {
            return Err(AuthError::InvalidCredentials(
                "TOTP digits must be between 6 and 8".to_owned(),
            ));
        }
        if config.period_secs == 0 {
            return Err(AuthError::InvalidCredentials(
                "TOTP period_secs must be positive".to_owned(),
            ));
        }
        Ok(())
    }

    /// Generates a fresh 20-byte CSPRNG TOTP secret.
    pub fn generate(config: TotpConfig) -> Self {
        let mut bytes = vec![0u8; 20];
        rand::rng().fill_bytes(&mut bytes);
        Self {
            bytes: Zeroizing::new(bytes),
            config,
        }
    }

    /// Creates a [`TotpSecret`] from raw secret bytes.
    pub fn from_bytes(bytes: Vec<u8>, config: TotpConfig) -> Result<Self, AuthError> {
        Self::validate_config(&config)?;
        if bytes.is_empty() {
            return Err(AuthError::InvalidCredentials(
                "TOTP secret cannot be empty".to_owned(),
            ));
        }
        Ok(Self {
            bytes: Zeroizing::new(bytes),
            config,
        })
    }

    /// Decodes a Base32-encoded TOTP secret (case-insensitive, no padding required).
    pub fn from_base32(encoded: &str, config: TotpConfig) -> Result<Self, AuthError> {
        let bytes = base32_decode(encoded)?;
        Self::from_bytes(bytes, config)
    }

    /// Encodes the secret as unpadded RFC 4648 Base32.
    pub fn to_base32(&self) -> String {
        base32_encode(&self.bytes)
    }

    /// Generates an `otpauth://totp/...` provisioning URI.
    pub fn otpauth_uri(&self, issuer: &str, account: &str) -> String {
        let alg = match self.config.algorithm {
            TotpAlgorithm::Sha1 => "SHA1",
            TotpAlgorithm::Sha256 => "SHA256",
            TotpAlgorithm::Sha512 => "SHA512",
        };
        let enc_issuer = percent_encode(issuer);
        let enc_account = percent_encode(account);
        format!(
            "otpauth://totp/{enc_issuer}:{enc_account}?secret={}&issuer={enc_issuer}&algorithm={alg}&digits={}&period={}",
            self.to_base32(),
            self.config.digits,
            self.config.period_secs
        )
    }

    fn generate_code_for_step(&self, step: u64) -> Result<String, AuthError> {
        Self::validate_config(&self.config)?;
        let counter = step.to_be_bytes();
        let digest = match self.config.algorithm {
            TotpAlgorithm::Sha1 => {
                let mut mac =
                    Hmac::<Sha1>::new_from_slice(&self.bytes).expect("HMAC accepts any key");
                mac.update(&counter);
                mac.finalize().into_bytes().to_vec()
            }
            TotpAlgorithm::Sha256 => {
                let mut mac =
                    Hmac::<Sha256>::new_from_slice(&self.bytes).expect("HMAC accepts any key");
                mac.update(&counter);
                mac.finalize().into_bytes().to_vec()
            }
            TotpAlgorithm::Sha512 => {
                let mut mac =
                    Hmac::<Sha512>::new_from_slice(&self.bytes).expect("HMAC accepts any key");
                mac.update(&counter);
                mac.finalize().into_bytes().to_vec()
            }
        };

        let offset = (digest[digest.len() - 1] & 0x0f) as usize;
        let code_num = u32::from_be_bytes([
            digest[offset] & 0x7f,
            digest[offset + 1],
            digest[offset + 2],
            digest[offset + 3],
        ]);
        let modulo = 10_u32.pow(self.config.digits);
        Ok(format!(
            "{:0width$}",
            code_num % modulo,
            width = self.config.digits as usize
        ))
    }

    /// Generates the TOTP code for `timestamp_secs`.
    pub fn generate_code(&self, timestamp_secs: u64) -> Result<String, AuthError> {
        Self::validate_config(&self.config)?;
        let step = timestamp_secs / self.config.period_secs;
        self.generate_code_for_step(step)
    }

    /// Verifies a TOTP code within the configured skew window, returning the matched step.
    pub fn verify_code(&self, code: &str, timestamp_secs: u64) -> Option<u64> {
        if Self::validate_config(&self.config).is_err() {
            return None;
        }
        let current_step = timestamp_secs / self.config.period_secs;
        let min_step = current_step.saturating_sub(self.config.skew_steps);
        let max_step = current_step.saturating_add(self.config.skew_steps);

        let mut matched = None;
        for step in min_step..=max_step {
            if let Ok(expected) = self.generate_code_for_step(step)
                && safe_equal(expected.as_bytes(), code.as_bytes())
            {
                matched = Some(step);
            }
        }
        matched
    }
}

/// Set of hashed single-use MFA recovery codes.
#[derive(Debug, Clone)]
pub struct RecoveryCodeSet {
    hashed_codes: Vec<[u8; 32]>,
    salt: [u8; 16],
}

impl RecoveryCodeSet {
    fn normalize_code(code: &str) -> String {
        code.chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .map(|c| c.to_ascii_uppercase())
            .collect()
    }

    fn hash_code(salt: &[u8; 16], normalized: &str) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(salt);
        hasher.update(normalized.as_bytes());
        hasher.finalize().into()
    }

    /// Generates `count` recovery codes, returning `(self, plaintext_codes)`.
    pub fn generate(count: usize) -> (Self, Vec<String>) {
        let mut rng = rand::rng();
        let mut salt = [0u8; 16];
        rng.fill_bytes(&mut salt);

        let mut hashed_codes = Vec::with_capacity(count);
        let mut plaintext_codes = Vec::with_capacity(count);
        let mut raw = [0u8; 12];

        for _ in 0..count {
            rng.fill_bytes(&mut raw);
            let chars: Vec<char> = raw
                .iter()
                .map(|b| RECOVERY_ALPHABET[(b & 0x1f) as usize] as char)
                .collect();
            let formatted = format!(
                "{}-{}-{}",
                chars[0..4].iter().collect::<String>(),
                chars[4..8].iter().collect::<String>(),
                chars[8..12].iter().collect::<String>()
            );
            let normalized = Self::normalize_code(&formatted);
            hashed_codes.push(Self::hash_code(&salt, &normalized));
            plaintext_codes.push(formatted);
        }

        (Self { hashed_codes, salt }, plaintext_codes)
    }

    /// Verifies and consumes a recovery code in constant time.
    pub fn verify_and_consume(&mut self, code: &str) -> bool {
        let normalized = Self::normalize_code(code);
        if normalized.is_empty() {
            return false;
        }
        let candidate = Self::hash_code(&self.salt, &normalized);

        let mut matched_idx = None;
        for (idx, stored) in self.hashed_codes.iter().enumerate() {
            if safe_equal(stored, candidate) {
                matched_idx = Some(idx);
            }
        }

        if let Some(idx) = matched_idx {
            self.hashed_codes.swap_remove(idx);
            true
        } else {
            false
        }
    }

    /// Returns the number of remaining unused recovery codes.
    pub fn remaining(&self) -> usize {
        self.hashed_codes.len()
    }
}

fn base32_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity((data.len() * 8).div_ceil(5));
    let mut buffer: u16 = 0;
    let mut bits_left: u8 = 0;

    for &byte in data {
        buffer = (buffer << 8) | u16::from(byte);
        bits_left += 8;
        while bits_left >= 5 {
            bits_left -= 5;
            let idx = ((buffer >> bits_left) & 0x1f) as usize;
            out.push(BASE32_ALPHABET[idx] as char);
        }
    }
    if bits_left > 0 {
        let idx = ((buffer << (5 - bits_left)) & 0x1f) as usize;
        out.push(BASE32_ALPHABET[idx] as char);
    }
    out
}

fn base32_decode(encoded: &str) -> Result<Vec<u8>, AuthError> {
    let mut out = Vec::with_capacity((encoded.len() * 5) / 8);
    let mut buffer: u16 = 0;
    let mut bits_left: u8 = 0;

    for ch in encoded.bytes() {
        if ch.is_ascii_whitespace() || ch == b'=' {
            continue;
        }
        let val = match ch.to_ascii_uppercase() {
            b'A'..=b'Z' => ch.to_ascii_uppercase() - b'A',
            b'2'..=b'7' => ch - b'2' + 26,
            _ => {
                return Err(AuthError::InvalidCredentials(
                    "invalid Base32 character in TOTP secret".to_owned(),
                ));
            }
        };
        buffer = (buffer << 5) | u16::from(val);
        bits_left += 5;
        if bits_left >= 8 {
            bits_left -= 8;
            out.push(((buffer >> bits_left) & 0xff) as u8);
        }
    }
    Ok(out)
}

fn percent_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(byte as char);
        } else {
            out.push('%');
            out.push_str(&format!("{byte:02X}"));
        }
    }
    out
}

/// WebAuthn authentication challenge issued to a client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebAuthnChallenge {
    /// Challenge bytes.
    pub challenge: Vec<u8>,
    /// Relying Party ID.
    pub rp_id: String,
    /// User identifier.
    pub user_id: String,
}

/// WebAuthn assertion payload submitted by a client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebAuthnAssertion {
    /// Credential ID.
    pub credential_id: Vec<u8>,
    /// Authenticator data bytes.
    pub authenticator_data: Vec<u8>,
    /// Client data JSON bytes.
    pub client_data_json: Vec<u8>,
    /// Signature bytes.
    pub signature: Vec<u8>,
}

/// Result of verifying a WebAuthn assertion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebAuthnVerification {
    /// Verified user ID.
    pub user_id: String,
    /// Updated signature counter.
    pub sign_count: u32,
}

/// Pluggable async boundary for WebAuthn / FIDO2 assertion verification.
pub trait WebAuthnVerifier: Send + Sync + 'static {
    /// Verifies a WebAuthn assertion against a previously issued challenge.
    fn verify_assertion<'a>(
        &'a self,
        challenge: &'a WebAuthnChallenge,
        assertion: &'a WebAuthnAssertion,
    ) -> BoxFuture<'a, Result<WebAuthnVerification, AuthError>>;
}

#[cfg(test)]
mod tests {
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
}
