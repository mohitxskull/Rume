//! SAML 2.0 assertion parsing, cryptographic XML-DSig verification, and replay protection.
//!
//! # Cryptographic & Replay-Protection Architecture
//!
//! - [`XmlDsigSamlVerifier`] provides production-grade XML-DSig verification backed by
//!   `bergshamra` (`rustcrypto`), supporting enveloped RSA-SHA256 and ECDSA-SHA256 signatures,
//!   Exclusive C14N canonicalization, pinned X.509 certificate trust stores, explicit runtime
//!   algorithm allow-listing (rejecting SHA-1/MD5), and anti-XSW `<Assertion>` ID binding.
//! - [`AsyncSamlReplayCache`] and [`RedisSamlReplayCache`] (with the `redis` feature) provide
//!   distributed atomic `SET NX EX` assertion replay protection alongside
//!   [`SamlParser::validate_browser_sso_async`].
//! - [`saml-rs`](https://crates.io/crates/saml-rs) supplies SAML 2.0 and XML-DSig namespace URN
//!   constants (`saml_rs::constants::namespace::{ASSERTION, DSIG, PROTOCOL}`). Structural XML
//!   parsing is handled by `quick-xml` (namespace-aware `NsReader`), and strict RFC 3339 /
//!   ISO 8601 instant validation is handled by `chrono`.

mod assertion;
mod discovery;
pub mod redis_replay;
mod verify;
pub mod xmldsig;

pub use assertion::{SamlAssertion, SamlParser, SamlSubjectConfirmationData, parse_iso8601_strict};
#[cfg(feature = "redis")]
pub use redis_replay::RedisSamlReplayCache;
pub use redis_replay::{AsyncSamlReplayCache, SyncReplayCacheAdapter};
pub use verify::{
    InMemorySamlReplayCache, SamlError, SamlReplayCache, SamlSignatureVerifier,
    SamlValidationConfig,
};
pub use xmldsig::XmlDsigSamlVerifier;

#[cfg(test)]
mod tests;
