//! Argon2id password hashing and verification.

use argon2::{
    ARGON2ID_IDENT, Algorithm, Argon2, Params, PasswordHasher as _, PasswordVerifier as _, Version,
    password_hash::phc::{Decimal, PasswordHash},
};
use secrecy::{ExposeSecret, SecretString};

use crate::auth::error::AuthError;

/// Configuration for Argon2id password hashing.
#[derive(Clone)]
pub struct PasswordHasherConfig {
    /// Memory cost in KiB (OWASP 2024 default: `19_456`).
    // Why: OWASP Password Storage Cheat Sheet minimum recommended Argon2id
    // configuration is 19 MiB (19_456 KiB), 2 iterations, 1 thread.
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

    fn build_argon2(&self) -> Result<Argon2<'_>, AuthError> {
        let params = Params::new(
            self.config.m_cost,
            self.config.t_cost,
            self.config.p_cost,
            Some(self.config.output_len),
        )
        .map_err(|e| AuthError::ProviderError(format!("invalid argon2id parameters: {e}")))?;

        match &self.config.pepper {
            Some(pepper) => Argon2::new_with_secret(
                pepper.expose_secret().as_bytes(),
                Algorithm::Argon2id,
                Version::V0x13,
                params,
            )
            .map_err(|e| AuthError::ProviderError(format!("invalid argon2id pepper: {e}"))),
            None => Ok(Argon2::new(Algorithm::Argon2id, Version::V0x13, params)),
        }
    }

    /// Hashes a password into a PHC string using Argon2id and a fresh CSPRNG salt.
    pub fn hash_password(&self, password: &SecretString) -> Result<String, AuthError> {
        let argon2 = self.build_argon2()?;
        argon2
            .hash_password(password.expose_secret().as_bytes())
            .map(|hash| hash.to_string())
            .map_err(|e| AuthError::ProviderError(format!("argon2id hash failed: {e}")))
    }

    /// Verifies a password against a PHC string in constant time.
    pub fn verify_password(
        &self,
        password: &SecretString,
        phc_hash: &str,
    ) -> Result<bool, AuthError> {
        let parsed = PasswordHash::new(phc_hash)
            .map_err(|_| AuthError::InvalidCredentials("malformed password hash".to_owned()))?;

        if parsed.algorithm != ARGON2ID_IDENT {
            return Err(AuthError::InvalidCredentials(
                "unsupported password hash algorithm".to_owned(),
            ));
        }

        let argon2 = self.build_argon2()?;
        match argon2.verify_password(password.expose_secret().as_bytes(), &parsed) {
            Ok(()) => Ok(true),
            Err(argon2::password_hash::Error::PasswordInvalid) => Ok(false),
            Err(e) => Err(AuthError::InvalidCredentials(format!(
                "password verification failed: {e}"
            ))),
        }
    }

    /// Returns `true` if `phc_hash` uses parameters weaker than or different from `self.config`.
    pub fn needs_rehash(&self, phc_hash: &str) -> bool {
        let Ok(parsed) = PasswordHash::new(phc_hash) else {
            return true;
        };

        if parsed.algorithm != ARGON2ID_IDENT {
            return true;
        }

        if parsed.version != Some(Decimal::from(Version::V0x13 as u32)) {
            return true;
        }

        let Ok(params) = Params::try_from(&parsed) else {
            return true;
        };

        let hash_len_matches = parsed
            .hash
            .as_ref()
            .is_some_and(|output| output.len() == self.config.output_len);

        params.m_cost() != self.config.m_cost
            || params.t_cost() != self.config.t_cost
            || params.p_cost() != self.config.p_cost
            || !hash_len_matches
    }

    /// Async wrapper executing [`Self::hash_password`] on Tokio's blocking pool.
    pub async fn hash_password_async(&self, password: SecretString) -> Result<String, AuthError> {
        let hasher = self.clone();
        tokio::task::spawn_blocking(move || hasher.hash_password(&password))
            .await
            .map_err(|e| AuthError::ProviderError(format!("password hash task failed: {e}")))?
    }

    /// Async wrapper executing [`Self::verify_password`] on Tokio's blocking pool.
    pub async fn verify_password_async(
        &self,
        password: SecretString,
        phc_hash: String,
    ) -> Result<bool, AuthError> {
        let hasher = self.clone();
        tokio::task::spawn_blocking(move || hasher.verify_password(&password, &phc_hash))
            .await
            .map_err(|e| AuthError::ProviderError(format!("password verify task failed: {e}")))?
    }
}

#[cfg(test)]
#[path = "password_tests.rs"]
mod tests;
