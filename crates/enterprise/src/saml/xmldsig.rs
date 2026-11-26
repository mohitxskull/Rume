//! Cryptographic XML-DSig verifier for SAML 2.0 assertions.

#![allow(dead_code, unused_variables)]

use super::verify::{SamlError, SamlSignatureVerifier};

/// Cryptographic XML-DSig verifier backed by `bergshamra` (`rustcrypto`) and pinned X.509 certs.
#[derive(Debug, Clone)]
pub struct XmlDsigSamlVerifier {
    trusted_der_certs: Vec<Vec<u8>>,
}

impl XmlDsigSamlVerifier {
    /// Creates a verifier trusting a single PEM-encoded X.509 certificate.
    pub fn from_pem_cert(pem: &str) -> Result<Self, SamlError> {
        todo!("implemented in Task 06")
    }

    /// Creates a verifier trusting a single DER-encoded X.509 certificate.
    pub fn from_der_cert(der: Vec<u8>) -> Result<Self, SamlError> {
        todo!("implemented in Task 06")
    }

    /// Creates a verifier trusting multiple PEM-encoded X.509 certificates (for key rollover).
    pub fn from_pem_certs(pems: &[&str]) -> Result<Self, SamlError> {
        todo!("implemented in Task 06")
    }
}

impl SamlSignatureVerifier for XmlDsigSamlVerifier {
    fn verify_assertion_signature(
        &self,
        raw_xml: &str,
        assertion_id: &str,
        assertion_xml: &str,
    ) -> Result<(), SamlError> {
        todo!("implemented in Task 06")
    }
}
