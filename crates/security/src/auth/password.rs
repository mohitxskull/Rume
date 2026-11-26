//! Argon2id password hashing and verification.

#![allow(dead_code, unused_variables)]

use secrecy::SecretString;

use crate::auth::error::AuthError;

/// Configuration for Argon2id password hashing.
#[derive(Clone)]
pub struct PasswordHasherConfig {
    /// Memory cost in KiB (OWASP 2024 default: `19_456`).
    pub m_cost: u32,
    /// Time cost / iterations (OWASP 2024 default: `2`).
    pub t_cost: u32,
    /// Parallelism lanes (OWASP 2024 default: `1`).
    pub p_cost: u32,
    /// Output hash length in bytes (default: `32`).
    pub output_len: usize,
    /// Optional application-wide secret pepper.
    pub pepper: Option<SecretString>,
}

impl std::fmt::Debug for PasswordHasherConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PasswordHasherConfig")
            .field("m_cost", &self.m_cost)
            .field("t_cost", &self.t_cost)
            .field("p_cost", &self.p_cost)
            .field("output_len", &self.output_len)
            .field("pepper", &self.pepper.as_ref().map(|_| "[REDACTED]"))
            .finish()
    }
}

impl Default for PasswordHasherConfig {
    fn default() -> Self {
        Self {
            m_cost: 19_456,
            t_cost: 2,
            p_cost: 1,
            output_len: 32,
            pepper: None,
        }
    }
}

impl PasswordHasherConfig {
    /// Low-cost Argon2id configuration intended **only** for fast unit tests.
    pub fn low_cost_for_tests() -> Self {
        Self {
            m_cost: 1024,
            t_cost: 1,
            p_cost: 1,
            output_len: 32,
            pepper: None,
        }
    }
}

/// Argon2id password hasher and verifier.
#[derive(Debug, Clone, Default)]
pub struct PasswordHasher {
    config: PasswordHasherConfig,
}

impl PasswordHasher {
    /// Creates a new [`PasswordHasher`] with the given configuration.
    pub fn new(config: PasswordHasherConfig) -> Self {
        Self { config }
    }

    /// Hashes a password into a PHC string using Argon2id and a fresh CSPRNG salt.
    pub fn hash_password(&self, password: &SecretString) -> Result<String, AuthError> {
        todo!("implemented in Task 02")
    }

    /// Verifies a password against a PHC string in constant time.
    pub fn verify_password(
        &self,
        password: &SecretString,
        phc_hash: &str,
    ) -> Result<bool, AuthError> {
        todo!("implemented in Task 02")
    }

    /// Returns `true` if `phc_hash` uses parameters weaker than or different from `self.config`.
    pub fn needs_rehash(&self, phc_hash: &str) -> bool {
        todo!("implemented in Task 02")
    }

    /// Async wrapper executing [`Self::hash_password`] on Tokio's blocking pool.
    pub async fn hash_password_async(&self, password: SecretString) -> Result<String, AuthError> {
        todo!("implemented in Task 02")
    }

    /// Async wrapper executing [`Self::verify_password`] on Tokio's blocking pool.
    pub async fn verify_password_async(
        &self,
        password: SecretString,
        phc_hash: String,
    ) -> Result<bool, AuthError> {
        todo!("implemented in Task 02")
    }
}
