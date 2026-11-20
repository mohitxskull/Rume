//! SAML 2.0 assertion parsing, claim extraction, and condition validation.
//!
//! **Stability: Experimental.**
//! This module is experimental while cryptographic trust boundaries and XMLDSig
//! verification continue hardening.
//!
//! > [!WARNING]
//! > **Non-Production Experimental Status**: The `SamlSignatureVerifier` interface mandates exact-assertion
//! > binding, but built-in implementations are development/test doubles. Do not promote or rely on this
//! > module as a production-supported authentication path until genuine cryptographic XMLDSig verification
//! > against real IdP certificates and adversarial wrapping test fixtures is formally certified.
//!
//! # Dependency basis
//!
//! [`saml-rs`](https://crates.io/crates/saml-rs) (v0.8.0) supplies only the SAML 2.0 and
//! XML-DSig namespace URN constants (`saml_rs::constants::namespace::{ASSERTION, DSIG, PROTOCOL}`).
//! Structural XML parsing is handled by `quick-xml` (namespace-aware `NsReader`), strict
//! RFC 3339 / ISO 8601 instant validation is handled by `chrono`, and cryptographic XML-DSig
//! verification is delegated to the caller-supplied [`SamlSignatureVerifier`] trait.
//!
//! Why: the high-level `saml_rs::Saml` browser-SSO API is intentionally not used because it
//! requires an `SpConfig`, signed `IdpDescriptor` metadata, a `ReplayCache`, a
//! `SamlValidationContext`, and HTTP form bindings that do not match this module's standalone
//! assertion / injected-verifier API. Canonicalization (`exc-c14n`) and X.509 RSA-SHA256
//! verification are intentionally not hand-rolled here; callers must supply a real
//! [`SamlSignatureVerifier`] implementation, and the `saml` feature remains Experimental.

mod assertion;
mod discovery;
mod verify;

pub use assertion::{SamlAssertion, SamlParser, SamlSubjectConfirmationData, parse_iso8601_strict};
pub use verify::{
    InMemorySamlReplayCache, SamlError, SamlReplayCache, SamlSignatureVerifier,
    SamlValidationConfig,
};

#[cfg(test)]
mod tests;
