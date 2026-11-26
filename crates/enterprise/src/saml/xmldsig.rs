//! Cryptographic XML-DSig verifier for SAML 2.0 assertions.

use std::collections::HashSet;

use bergshamra::core::{algorithm, ns};
use bergshamra::dsig::verify::verify_all_document_with_source;
use bergshamra::keys::loader::{load_x509_cert_der, load_x509_cert_pem};
use bergshamra::xml::uppsala::{self, Document, NodeId};
use bergshamra::{DsigContext, KeysManager, VerifyResult};

use super::verify::{SamlError, SamlSignatureVerifier};

const SAML2_ASSERTION_NS: &str = saml_rs::constants::namespace::ASSERTION;

/// Cryptographic XML-DSig verifier backed by `bergshamra` (`rustcrypto`) and pinned X.509 certs.
#[derive(Debug, Clone)]
pub struct XmlDsigSamlVerifier {
    trusted_der_certs: Vec<Vec<u8>>,
}

impl XmlDsigSamlVerifier {
    /// Creates a verifier trusting a single PEM-encoded X.509 certificate.
    pub fn from_pem_cert(pem: &str) -> Result<Self, SamlError> {
        Self::from_pem_certs(&[pem])
    }

    /// Creates a verifier trusting a single DER-encoded X.509 certificate.
    pub fn from_der_cert(der: Vec<u8>) -> Result<Self, SamlError> {
        if der.is_empty() {
            return Err(SamlError::InvalidSignature(
                "trusted X.509 DER certificate must not be empty".into(),
            ));
        }
        load_x509_cert_der(&der).map_err(|e| {
            SamlError::InvalidSignature(format!("invalid trusted X.509 DER certificate: {e}"))
        })?;
        Ok(Self {
            trusted_der_certs: vec![der],
        })
    }

    /// Creates a verifier trusting multiple PEM-encoded X.509 certificates (for key rollover).
    pub fn from_pem_certs(pems: &[&str]) -> Result<Self, SamlError> {
        if pems.is_empty() {
            return Err(SamlError::InvalidSignature(
                "at least one trusted X.509 certificate is required".into(),
            ));
        }
        let mut trusted_der_certs = Vec::with_capacity(pems.len());
        for pem in pems {
            let key = load_x509_cert_pem(pem.as_bytes()).map_err(|e| {
                SamlError::InvalidSignature(format!("invalid trusted X.509 PEM certificate: {e}"))
            })?;
            let der = key.x509_chain.into_iter().next().ok_or_else(|| {
                SamlError::InvalidSignature(
                    "parsed X.509 PEM certificate did not yield DER bytes".into(),
                )
            })?;
            trusted_der_certs.push(der);
        }
        Ok(Self { trusted_der_certs })
    }
}

impl SamlSignatureVerifier for XmlDsigSamlVerifier {
    fn verify_assertion_signature(
        &self,
        raw_xml: &str,
        assertion_id: &str,
        _assertion_xml: &str,
    ) -> Result<(), SamlError> {
        if assertion_id.is_empty() {
            return Err(SamlError::InvalidSignature(
                "assertion ID must not be empty".into(),
            ));
        }

        let doc = uppsala::parse(raw_xml)
            .map_err(|e| SamlError::Malformed(format!("invalid XML document: {e}")))?;

        reject_duplicate_ids(&doc)?;

        let mut keys_manager = KeysManager::new();
        for der in &self.trusted_der_certs {
            let key = load_x509_cert_der(der).map_err(|e| {
                SamlError::InvalidSignature(format!("failed to load pinned X.509 cert: {e}"))
            })?;
            keys_manager.add_key(key);
            keys_manager.add_trusted_cert(der.clone());
        }

        // Why: `DsigContext::new` sets `trusted_keys_only = true`, `strict_verification = true`,
        // and `require_reference_digests = true`, so attacker-embedded `<KeyInfo>` certs are
        // never trusted unless they match a pinned certificate above.
        let ctx = DsigContext::new(keys_manager);
        let results = verify_all_document_with_source(&ctx, &doc, Some(raw_xml)).map_err(|e| {
            let msg = e.to_string();
            if msg.to_ascii_lowercase().contains("duplicate") {
                SamlError::SignatureWrappingDetected(msg)
            } else {
                SamlError::InvalidSignature(msg)
            }
        })?;

        let expected_uri = format!("#{assertion_id}");
        let mut any_valid_signature = false;
        let mut last_alg_err: Option<String> = None;
        let mut last_invalid_reason: Option<String> = None;

        for result in &results {
            match result {
                VerifyResult::Valid {
                    signature_node,
                    references,
                    ..
                } => {
                    if !result.all_reference_digests_verified() {
                        continue;
                    }
                    if let Err(reason) = validate_signature_algorithms(&doc, *signature_node) {
                        last_alg_err = Some(reason);
                        continue;
                    }
                    any_valid_signature = true;

                    let covers_assertion = references.iter().any(|r| {
                        r.digest_verified
                            && r.uri == expected_uri
                            && r.resolved_node
                                .is_some_and(|node| is_target_assertion(&doc, node, assertion_id))
                    });
                    if covers_assertion {
                        return Ok(());
                    }
                }
                VerifyResult::Invalid { reason } => {
                    last_invalid_reason = Some(reason.clone());
                }
            }
        }

        if let Some(reason) = last_alg_err {
            return Err(SamlError::InvalidSignature(reason));
        }
        if any_valid_signature {
            return Err(SamlError::SignatureWrappingDetected(format!(
                "no valid signature covers Assertion ID '{assertion_id}'"
            )));
        }
        Err(SamlError::InvalidSignature(
            last_invalid_reason.unwrap_or_else(|| "no valid XML-DSig signature found".into()),
        ))
    }
}

fn reject_duplicate_ids(doc: &Document<'_>) -> Result<(), SamlError> {
    let mut seen_ids = HashSet::new();
    for node_id in doc.descendants(doc.root()) {
        let Some(el) = doc.element(node_id) else {
            continue;
        };
        for attr in &el.attributes {
            if matches!(
                attr.name.local_name.as_ref(),
                "ID" | "Id" | "id" | "AssertionID"
            ) && !seen_ids.insert(attr.value.as_ref())
            {
                return Err(SamlError::SignatureWrappingDetected(format!(
                    "duplicate XML ID attribute '{}'",
                    attr.value
                )));
            }
        }
    }
    Ok(())
}

fn is_target_assertion(doc: &Document<'_>, node: NodeId, assertion_id: &str) -> bool {
    let Some(el) = doc.element(node) else {
        return false;
    };
    el.name.local_name.as_ref() == "Assertion"
        && el.name.namespace_uri.as_deref() == Some(SAML2_ASSERTION_NS)
        && el.get_attribute("ID") == Some(assertion_id)
}

fn validate_signature_algorithms(doc: &Document<'_>, sig_node: NodeId) -> Result<(), String> {
    let signed_info = find_dsig_child(doc, sig_node, "SignedInfo")
        .ok_or_else(|| "missing <ds:SignedInfo> in <ds:Signature>".to_string())?;

    let c14n_alg = find_dsig_child(doc, signed_info, "CanonicalizationMethod")
        .and_then(|n| doc.get_attribute(n, "Algorithm"))
        .ok_or_else(|| "missing <ds:CanonicalizationMethod> Algorithm".to_string())?;
    if !is_allowed_c14n(c14n_alg) {
        return Err(format!(
            "unsupported or disallowed CanonicalizationMethod '{c14n_alg}'"
        ));
    }

    let sig_alg = find_dsig_child(doc, signed_info, "SignatureMethod")
        .and_then(|n| doc.get_attribute(n, "Algorithm"))
        .ok_or_else(|| "missing <ds:SignatureMethod> Algorithm".to_string())?;
    if sig_alg != algorithm::RSA_SHA256 && sig_alg != algorithm::ECDSA_SHA256 {
        return Err(format!(
            "unsupported or disallowed SignatureMethod '{sig_alg}'"
        ));
    }

    let mut saw_reference = false;
    for child in doc.children(signed_info) {
        let Some(el) = doc.element(child) else {
            continue;
        };
        if el.name.namespace_uri.as_deref() != Some(ns::DSIG)
            || el.name.local_name.as_ref() != "Reference"
        {
            continue;
        }
        saw_reference = true;

        let digest_alg = find_dsig_child(doc, child, "DigestMethod")
            .and_then(|n| doc.get_attribute(n, "Algorithm"))
            .ok_or_else(|| "missing <ds:DigestMethod> Algorithm".to_string())?;
        if digest_alg != algorithm::SHA256 {
            return Err(format!(
                "unsupported or disallowed DigestMethod '{digest_alg}'"
            ));
        }

        if let Some(transforms) = find_dsig_child(doc, child, "Transforms") {
            for t_node in doc.children(transforms) {
                let Some(t_el) = doc.element(t_node) else {
                    continue;
                };
                if t_el.name.namespace_uri.as_deref() != Some(ns::DSIG)
                    || t_el.name.local_name.as_ref() != "Transform"
                {
                    continue;
                }
                let t_alg = t_el
                    .get_attribute("Algorithm")
                    .ok_or_else(|| "missing <ds:Transform> Algorithm".to_string())?;
                if t_alg != algorithm::ENVELOPED_SIGNATURE && !is_allowed_c14n(t_alg) {
                    return Err(format!("unsupported or disallowed Transform '{t_alg}'"));
                }
            }
        }
    }

    if !saw_reference {
        return Err("missing <ds:Reference> in <ds:SignedInfo>".to_string());
    }
    Ok(())
}

fn find_dsig_child(doc: &Document<'_>, parent: NodeId, local_name: &str) -> Option<NodeId> {
    doc.children(parent).into_iter().find(|&id| {
        doc.element(id).is_some_and(|el| {
            el.name.namespace_uri.as_deref() == Some(ns::DSIG)
                && el.name.local_name.as_ref() == local_name
        })
    })
}

fn is_allowed_c14n(alg: &str) -> bool {
    alg == algorithm::EXC_C14N || alg == algorithm::EXC_C14N_WITH_COMMENTS
}
