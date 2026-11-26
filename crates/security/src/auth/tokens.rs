//! Expiring HMAC-SHA256 signed tokens and signed URLs.

#![allow(dead_code, unused_variables)]

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use url::Url;
use zeroize::Zeroizing;

use crate::auth::error::AuthError;

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
        todo!("implemented in Task 02")
    }

    /// Adds a previous signing key for zero-downtime key rotation.
    pub fn with_previous_key(self, key: impl AsRef<[u8]>) -> Result<Self, AuthError> {
        todo!("implemented in Task 02")
    }

    /// Signs a token for `purpose` and `subject` expiring at `expires_at`.
    pub fn sign(&self, claims: &SignedTokenClaims) -> Result<String, AuthError> {
        todo!("implemented in Task 02")
    }

    /// Verifies a token's signature, expiry, and purpose binding in constant time.
    pub fn verify(
        &self,
        token: &str,
        expected_purpose: &str,
        now_epoch_sec: u64,
    ) -> Result<SignedTokenClaims, AuthError> {
        todo!("implemented in Task 02")
    }
}

/// Helper for generating and verifying HMAC-SHA256 signed URLs.
#[derive(Debug, Clone)]
pub struct SignedUrl;

impl SignedUrl {
    /// Appends `expires` and `sig` query parameters to `url`.
    pub fn sign(
        signer: &TokenSigner,
        method: &str,
        url: &Url,
        expires_at: u64,
    ) -> Result<Url, AuthError> {
        todo!("implemented in Task 02")
    }

    /// Verifies a signed URL's `expires` and `sig` query parameters in constant time.
    pub fn verify(
        signer: &TokenSigner,
        method: &str,
        url: &Url,
        now_epoch_sec: u64,
    ) -> Result<(), AuthError> {
        todo!("implemented in Task 02")
    }
}
