//! Expiring HMAC-SHA256 signed tokens and signed URLs.

use std::collections::BTreeMap;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, KeyInit, Mac};
use rume_foundation::util::safe_equal;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use subtle::ConstantTimeEq;
use url::{Url, form_urlencoded};
use zeroize::Zeroizing;

use crate::auth::error::AuthError;

type HmacSha256 = Hmac<Sha256>;

/// Minimum HMAC-SHA256 signing key length in bytes.
// Why: 32 bytes (256 bits) matches HMAC-SHA256 output size to prevent weak-key brute force.
const MIN_SIGNING_KEY_LEN: usize = 32;

/// Claims embedded in a signed token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedTokenClaims {
    /// Purpose binding (e.g. `"email_verify"`, `"password_reset"`, `"download"`).
    pub purpose: String,
    /// Subject identifier (e.g. user ID).
    pub subject: String,
    /// Expiration timestamp (UNIX epoch seconds).
    pub expires_at: u64,
    /// Issuance timestamp (UNIX epoch seconds).
    pub issued_at: u64,
    /// Unique nonce for optional single-use replay tracking.
    pub nonce: String,
    /// Arbitrary key-value metadata.
    pub metadata: BTreeMap<String, String>,
}

/// HMAC-SHA256 token signer and verifier with key-rotation support.
#[derive(Clone)]
pub struct TokenSigner {
    primary_key: Zeroizing<Vec<u8>>,
    previous_keys: Vec<Zeroizing<Vec<u8>>>,
}

impl std::fmt::Debug for TokenSigner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenSigner")
            .field("primary_key", &"[REDACTED]")
            .field("previous_keys_count", &self.previous_keys.len())
            .finish()
    }
}

impl TokenSigner {
    /// Creates a new [`TokenSigner`] enforcing a minimum 32-byte key length.
    pub fn new(primary_key: impl AsRef<[u8]>) -> Result<Self, AuthError> {
        let key = primary_key.as_ref();
        if key.len() < MIN_SIGNING_KEY_LEN {
            return Err(AuthError::InvalidCredentials(format!(
                "signing key must be at least {MIN_SIGNING_KEY_LEN} bytes"
            )));
        }
        Ok(Self {
            primary_key: Zeroizing::new(key.to_vec()),
            previous_keys: Vec::new(),
        })
    }

    /// Adds a previous signing key for zero-downtime key rotation.
    pub fn with_previous_key(mut self, key: impl AsRef<[u8]>) -> Result<Self, AuthError> {
        let key = key.as_ref();
        if key.len() < MIN_SIGNING_KEY_LEN {
            return Err(AuthError::InvalidCredentials(format!(
                "previous signing key must be at least {MIN_SIGNING_KEY_LEN} bytes"
            )));
        }
        self.previous_keys.push(Zeroizing::new(key.to_vec()));
        Ok(self)
    }

    fn compute_mac(key: &[u8], message: &[u8]) -> [u8; 32] {
        let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts keys of any length");
        mac.update(message);
        mac.finalize().into_bytes().into()
    }

    fn verify_mac(&self, message: &[u8], provided_sig: &[u8]) -> bool {
        let mut matched = Self::compute_mac(&self.primary_key, message).ct_eq(provided_sig);
        for prev in &self.previous_keys {
            matched |= Self::compute_mac(prev, message).ct_eq(provided_sig);
        }
        bool::from(matched)
    }

    /// Signs a token for `purpose` and `subject` expiring at `expires_at`.
    pub fn sign(&self, claims: &SignedTokenClaims) -> Result<String, AuthError> {
        let payload = serde_json::to_vec(claims)
            .map_err(|e| AuthError::ProviderError(format!("token serialization failed: {e}")))?;
        let payload_b64 = URL_SAFE_NO_PAD.encode(payload);
        let sig = Self::compute_mac(&self.primary_key, payload_b64.as_bytes());
        let sig_b64 = URL_SAFE_NO_PAD.encode(sig);
        Ok(format!("{payload_b64}.{sig_b64}"))
    }

    /// Verifies a token's signature, expiry, and purpose binding in constant time.
    pub fn verify(
        &self,
        token: &str,
        expected_purpose: &str,
        now_epoch_sec: u64,
    ) -> Result<SignedTokenClaims, AuthError> {
        let (payload_b64, sig_b64) = token
            .split_once('.')
            .ok_or_else(|| AuthError::InvalidCredentials("malformed signed token".to_owned()))?;

        let provided_sig = URL_SAFE_NO_PAD
            .decode(sig_b64)
            .map_err(|_| AuthError::InvalidCredentials("invalid token signature".to_owned()))?;

        if !self.verify_mac(payload_b64.as_bytes(), &provided_sig) {
            return Err(AuthError::InvalidCredentials(
                "invalid token signature".to_owned(),
            ));
        }

        let payload_bytes = URL_SAFE_NO_PAD
            .decode(payload_b64)
            .map_err(|_| AuthError::InvalidCredentials("invalid token payload".to_owned()))?;
        let claims: SignedTokenClaims = serde_json::from_slice(&payload_bytes)
            .map_err(|_| AuthError::InvalidCredentials("malformed token claims".to_owned()))?;

        if !safe_equal(claims.purpose.as_bytes(), expected_purpose.as_bytes()) {
            return Err(AuthError::InvalidCredentials(
                "token purpose mismatch".to_owned(),
            ));
        }

        if now_epoch_sec >= claims.expires_at {
            return Err(AuthError::Expired);
        }

        Ok(claims)
    }
}

/// Helper for generating and verifying HMAC-SHA256 signed URLs.
#[derive(Debug, Clone)]
pub struct SignedUrl;

impl SignedUrl {
    fn canonical_message(method: &str, url: &Url, expires_at: u64) -> String {
        let mut pairs: Vec<(String, String)> = url
            .query_pairs()
            .filter(|(k, _)| k != "sig" && k != "expires")
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        pairs.sort();

        let mut serializer = form_urlencoded::Serializer::new(String::new());
        for (k, v) in &pairs {
            serializer.append_pair(k, v);
        }
        serializer.append_pair("expires", &expires_at.to_string());
        let query = serializer.finish();

        let scheme = url.scheme();
        let host = url.host_str().unwrap_or("").to_ascii_lowercase();
        let port = url
            .port_or_known_default()
            .map(|p| p.to_string())
            .unwrap_or_default();

        format!(
            "{}\n{}\n{}\n{}\n{}\n{}",
            method.to_ascii_uppercase(),
            scheme,
            host,
            port,
            url.path(),
            query
        )
    }

    /// Appends `expires` and `sig` query parameters to `url`.
    pub fn sign(
        signer: &TokenSigner,
        method: &str,
        url: &Url,
        expires_at: u64,
    ) -> Result<Url, AuthError> {
        let canonical = Self::canonical_message(method, url, expires_at);
        let sig = TokenSigner::compute_mac(&signer.primary_key, canonical.as_bytes());
        let sig_b64 = URL_SAFE_NO_PAD.encode(sig);

        let mut signed = url.clone();
        let existing: Vec<(String, String)> = url
            .query_pairs()
            .filter(|(k, _)| k != "sig" && k != "expires")
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();

        {
            let mut pairs = signed.query_pairs_mut();
            pairs.clear();
            for (k, v) in &existing {
                pairs.append_pair(k, v);
            }
            pairs.append_pair("expires", &expires_at.to_string());
            pairs.append_pair("sig", &sig_b64);
        }

        Ok(signed)
    }

    /// Verifies a signed URL's `expires` and `sig` query parameters in constant time.
    pub fn verify(
        signer: &TokenSigner,
        method: &str,
        url: &Url,
        now_epoch_sec: u64,
    ) -> Result<(), AuthError> {
        let mut expires_raw: Option<String> = None;
        let mut sig_raw: Option<String> = None;
        for (k, v) in url.query_pairs() {
            match k.as_ref() {
                "expires" => {
                    if expires_raw.is_some() {
                        return Err(AuthError::InvalidCredentials(
                            "duplicate expires parameter".to_owned(),
                        ));
                    }
                    expires_raw = Some(v.into_owned());
                }
                "sig" => {
                    if sig_raw.is_some() {
                        return Err(AuthError::InvalidCredentials(
                            "duplicate sig parameter".to_owned(),
                        ));
                    }
                    sig_raw = Some(v.into_owned());
                }
                _ => {}
            }
        }

        let expires_at: u64 = expires_raw
            .and_then(|s| s.parse::<u64>().ok())
            .ok_or_else(|| {
                AuthError::InvalidCredentials("missing or invalid expires".to_owned())
            })?;
        let sig_b64 = sig_raw
            .ok_or_else(|| AuthError::InvalidCredentials("missing URL signature".to_owned()))?;
        let provided_sig = URL_SAFE_NO_PAD
            .decode(sig_b64)
            .map_err(|_| AuthError::InvalidCredentials("invalid URL signature".to_owned()))?;

        let canonical = Self::canonical_message(method, url, expires_at);
        if !signer.verify_mac(canonical.as_bytes(), &provided_sig) {
            return Err(AuthError::InvalidCredentials(
                "invalid URL signature".to_owned(),
            ));
        }

        if now_epoch_sec >= expires_at {
            return Err(AuthError::Expired);
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY_1: &[u8] = b"0123456789abcdef0123456789abcdef";
    const KEY_2: &[u8] = b"fedcba9876543210fedcba9876543210";

    fn sample_claims(expires_at: u64) -> SignedTokenClaims {
        let mut metadata = BTreeMap::new();
        metadata.insert("role".to_owned(), "admin".to_owned());
        SignedTokenClaims {
            purpose: "email_verify".to_owned(),
            subject: "user-42".to_owned(),
            expires_at,
            issued_at: 1_000,
            nonce: "nonce-1".to_owned(),
            metadata,
        }
    }

    #[test]
    fn token_signer_rejects_short_key_tampered_payload_wrong_purpose_and_expired_token() {
        assert!(TokenSigner::new(b"too-short-key").is_err());

        let signer = TokenSigner::new(KEY_1).expect("32-byte key is valid");
        let dbg = format!("{signer:?}");
        assert!(dbg.contains("[REDACTED]"));

        let token = signer.sign(&sample_claims(2_000)).expect("sign succeeds");
        assert!(token.contains('.'));

        let verified = signer
            .verify(&token, "email_verify", 1_500)
            .expect("valid token verifies");
        assert_eq!(verified.subject, "user-42");
        assert_eq!(
            verified.metadata.get("role").map(String::as_str),
            Some("admin")
        );

        // Wrong purpose
        assert!(matches!(
            signer.verify(&token, "password_reset", 1_500),
            Err(AuthError::InvalidCredentials(_))
        ));

        // Expired at boundary (now >= expires_at)
        assert!(matches!(
            signer.verify(&token, "email_verify", 2_000),
            Err(AuthError::Expired)
        ));

        // Tampered payload
        let (payload, sig) = token.split_once('.').expect("dot separator");
        let mut bytes = payload.as_bytes().to_vec();
        bytes[0] = if bytes[0] == b'A' { b'B' } else { b'A' };
        let tampered = format!("{}.{sig}", String::from_utf8(bytes).unwrap());
        assert!(matches!(
            signer.verify(&tampered, "email_verify", 1_500),
            Err(AuthError::InvalidCredentials(_))
        ));
    }

    #[test]
    fn token_signer_supports_key_rotation_with_previous_keys() {
        let old_signer = TokenSigner::new(KEY_1).expect("key 1");
        let token = old_signer
            .sign(&sample_claims(2_000))
            .expect("sign with old key");

        // Short previous key must be rejected.
        assert!(
            TokenSigner::new(KEY_2)
                .unwrap()
                .with_previous_key(b"short")
                .is_err()
        );

        let rotated = TokenSigner::new(KEY_2)
            .expect("key 2")
            .with_previous_key(KEY_1)
            .expect("add previous key");

        let verified = rotated
            .verify(&token, "email_verify", 1_500)
            .expect("rotated signer verifies token signed with previous key");
        assert_eq!(verified.subject, "user-42");
    }

    #[test]
    fn signed_url_roundtrip_rejects_tampered_query_params_and_expired_url() {
        let signer = TokenSigner::new(KEY_1).expect("key 1");
        let base = Url::parse("https://example.com/files/doc.pdf?b=2&a=1").unwrap();

        let signed = SignedUrl::sign(&signer, "GET", &base, 5_000).expect("sign url");
        // Unrelated query params preserved.
        assert_eq!(
            signed
                .query_pairs()
                .find(|(k, _)| k == "a")
                .map(|(_, v)| v.into_owned()),
            Some("1".to_owned())
        );

        SignedUrl::verify(&signer, "GET", &signed, 4_999).expect("valid signed url");

        // Wrong HTTP method
        assert!(matches!(
            SignedUrl::verify(&signer, "POST", &signed, 4_999),
            Err(AuthError::InvalidCredentials(_))
        ));

        // Expired URL
        assert!(matches!(
            SignedUrl::verify(&signer, "GET", &signed, 5_000),
            Err(AuthError::Expired)
        ));

        // Tampered query param
        let tampered = Url::parse(&signed.as_str().replace("a=1", "a=999")).unwrap();
        assert!(matches!(
            SignedUrl::verify(&signer, "GET", &tampered, 4_999),
            Err(AuthError::InvalidCredentials(_))
        ));

        // Cross-host URL must be rejected
        let cross_host =
            Url::parse(&signed.as_str().replace("example.com", "b.example.com")).unwrap();
        assert!(matches!(
            SignedUrl::verify(&signer, "GET", &cross_host, 4_999),
            Err(AuthError::InvalidCredentials(_))
        ));

        // Scheme-downgrade URL must be rejected
        let downgraded = Url::parse(&signed.as_str().replacen("https://", "http://", 1)).unwrap();
        assert!(matches!(
            SignedUrl::verify(&signer, "GET", &downgraded, 4_999),
            Err(AuthError::InvalidCredentials(_))
        ));

        // Duplicate expires parameter must be rejected
        let dup_expires = Url::parse(&format!("{}&expires=9999999999", signed.as_str())).unwrap();
        assert!(matches!(
            SignedUrl::verify(&signer, "GET", &dup_expires, 4_999),
            Err(AuthError::InvalidCredentials(_))
        ));

        // Duplicate sig parameter must be rejected
        let dup_sig = Url::parse(&format!("{}&sig=deadbeef", signed.as_str())).unwrap();
        assert!(matches!(
            SignedUrl::verify(&signer, "GET", &dup_sig, 4_999),
            Err(AuthError::InvalidCredentials(_))
        ));
    }
}
