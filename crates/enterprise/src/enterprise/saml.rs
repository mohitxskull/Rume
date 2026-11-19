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

use quick_xml::events::Event;
use quick_xml::name::ResolveResult;
use quick_xml::reader::NsReader;
use saml_rs::constants::namespace::{ASSERTION, DSIG, PROTOCOL};
use std::collections::BTreeMap;
use thiserror::Error;

fn is_ns(res: &ResolveResult<'_>, expected: &str) -> bool {
    match res {
        ResolveResult::Bound(ns) => ns.as_ref() == expected,
        _ => false,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum XmlNs {
    Assertion,
    Protocol,
    Other,
}

#[derive(Default)]
struct AssertionFields {
    subject: Option<String>,
    audience: Option<String>,
    attributes: BTreeMap<String, Vec<String>>,
}

fn record_assertion_text_chunk(
    chunk: &str,
    element_stack: &[(String, XmlNs)],
    in_assertion: bool,
    active_issuer_buf: &mut Option<String>,
    current_attr_name: Option<&str>,
    fields: &mut AssertionFields,
) {
    let parent = element_stack.last().map(|(name, ns)| (name.as_str(), *ns));
    if matches!(parent, Some(("Issuer", XmlNs::Assertion)))
        && let Some(buf) = active_issuer_buf.as_mut()
    {
        buf.push_str(chunk);
        return;
    }

    if !in_assertion {
        return;
    }

    let text = chunk.trim();
    if text.is_empty() {
        return;
    }

    let grand_parent = element_stack
        .len()
        .checked_sub(2)
        .and_then(|idx| element_stack.get(idx))
        .map(|(name, ns)| (name.as_str(), *ns));

    if let Some((p_name, XmlNs::Assertion)) = parent {
        if p_name == "NameID"
            && fields.subject.is_none()
            && matches!(grand_parent, Some(("Subject", XmlNs::Assertion)))
        {
            fields.subject = Some(text.to_string());
        } else if p_name == "Audience" && fields.audience.is_none() {
            fields.audience = Some(text.to_string());
        } else if p_name == "AttributeValue"
            && let Some(attr_name) = current_attr_name
        {
            fields
                .attributes
                .entry(attr_name.to_string())
                .or_default()
                .push(text.to_string());
        }
    }
}

/// SAML processing and validation errors.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum SamlError {
    /// The XML structure was missing essential SAML tags or malformed.
    #[error("malformed SAML assertion: {0}")]
    Malformed(String),

    /// Assertion timestamp has expired.
    #[error("assertion expired at {not_on_or_after}, current timestamp is {current}")]
    Expired { not_on_or_after: u64, current: u64 },

    /// Assertion is not yet valid.
    #[error("assertion not valid before {not_before}, current timestamp is {current}")]
    NotYetValid { not_before: u64, current: u64 },

    /// The recipient audience did not match this service provider.
    #[error("audience mismatch: expected '{expected}', received '{received}'")]
    AudienceMismatch { expected: String, received: String },

    /// Digital signature verification failed.
    #[error("signature verification failed: {0}")]
    InvalidSignature(String),

    /// Missing mandatory signature verifier.
    #[error("signature verification required: missing signature verifier")]
    MissingSignatureVerifier,

    /// Identity provider issuer mismatch.
    #[error("issuer mismatch: expected '{expected}', received '{received}'")]
    IssuerMismatch { expected: String, received: String },

    /// SAML destination endpoint mismatch.
    #[error("destination mismatch: expected '{expected}', received '{received}'")]
    DestinationMismatch { expected: String, received: String },

    /// Possible XML signature wrapping attack detected.
    #[error("signature wrapping attack detected: {0}")]
    SignatureWrappingDetected(String),

    /// Missing mandatory expiration condition.
    #[error("missing mandatory NotOnOrAfter condition in SAML assertion")]
    MissingExpirationCondition,
}

/// Interface for verifying cryptographic signatures on SAML assertions.
pub trait SamlSignatureVerifier: Send + Sync {
    /// Verifies the digital signature on the raw XML assertion.
    fn verify_signature(&self, raw_xml: &str) -> Result<(), SamlError>;

    /// Verifies that the specific assertion node is cryptographically valid and covered by signature.
    ///
    /// Ensures that what was cryptographically validated matches the exact assertion node
    /// later consumed by the application shell, closing the trust-boundary gap.
    fn verify_assertion_signature(
        &self,
        raw_xml: &str,
        assertion_id: &str,
        assertion_xml: &str,
    ) -> Result<(), SamlError>;
}

/// A validated SAML 2.0 authentication assertion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SamlAssertion {
    /// Assertion ID.
    pub id: String,
    /// Identity provider issuer URL or entity ID.
    pub issuer: String,
    /// Authenticated subject (e.g. NameID email).
    pub subject: String,
    /// Audience restriction URI.
    pub audience: Option<String>,
    /// Destination endpoint URI.
    pub destination: Option<String>,
    /// NotBefore epoch seconds condition.
    pub not_before: Option<u64>,
    /// NotOnOrAfter epoch seconds condition.
    pub not_on_or_after: Option<u64>,
    /// Extracted user claims and attributes.
    pub attributes: BTreeMap<String, Vec<String>>,
}

/// Parses an ISO 8601 / RFC 3339 timestamp strictly, including UTC `'Z'`, fractional seconds,
/// and explicit colon-separated timezone offsets (`±HH:MM`).
///
/// An explicit timezone (`'Z'` or `±HH:MM`) and a `'T'`/`'t'` date-time separator are mandatory:
/// raw epoch integers, space-separated RFC 3339 timestamps, naive wall-clock strings without a
/// timezone, and pre-Unix-epoch instants are rejected fail-closed.
pub fn parse_iso8601_strict(s: &str) -> Result<u64, SamlError> {
    let trimmed = s.trim();

    // Why: delegate calendar, leap-year, and `-23:59..=+23:59` offset validation to
    // `chrono::DateTime::parse_from_rfc3339` (already pinned in `Cargo.lock`) instead of
    // hand-rolled date math and UTF-8 byte slicing that can panic on multi-byte inputs; check
    // byte index 10 for `'T'`/`'t'` to reject RFC 3339 space separators and use `u64::try_from`
    // so pre-1970 instants fail closed instead of wrapping.
    if !matches!(trimmed.as_bytes().get(10), Some(b'T' | b't')) {
        return Err(SamlError::Malformed(format!(
            "invalid ISO 8601 timestamp '{trimmed}': missing 'T' separator"
        )));
    }

    let dt = chrono::DateTime::parse_from_rfc3339(trimmed).map_err(|err| {
        SamlError::Malformed(format!("invalid ISO 8601 timestamp '{trimmed}': {err}"))
    })?;

    u64::try_from(dt.timestamp()).map_err(|_| {
        SamlError::Malformed(format!("timestamp '{trimmed}' resolves before Unix epoch"))
    })
}

/// Parser and validator for SAML 2.0 assertions.
pub struct SamlParser;

impl SamlParser {
    /// Extracts the `entityID` from an `<EntityDescriptor>` XML document.
    pub fn parse_entity_id(xml: &str) -> Result<String, SamlError> {
        let mut reader = NsReader::from_str(xml);
        reader.config_mut().trim_text(true);
        let mut buf = Vec::new();
        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                    let local_name = e.local_name();
                    if local_name.as_ref() == "EntityDescriptor" {
                        for attr in e.attributes().flatten() {
                            if attr.key.local_name().as_ref() == "entityID" {
                                return Ok(attr.value.as_ref().to_string());
                            }
                        }
                        return Err(SamlError::Malformed(
                            "missing entityID attribute on EntityDescriptor".into(),
                        ));
                    }
                }
                Ok(Event::Eof) => break,
                Err(err) => return Err(SamlError::Malformed(err.to_string())),
                _ => {}
            }
            buf.clear();
        }
        Err(SamlError::Malformed(
            "EntityDescriptor element not found".into(),
        ))
    }

    /// Parses an XML string containing a SAML 2.0 Assertion.
    ///
    /// Deprecated: Unverified assertion parsing in production is insecure.
    /// Prefer [`SamlParser::parse_verified`] with an injected [`SamlSignatureVerifier`].
    // Why: Zero workspace callers remain, but this `#[deprecated]` shim is retained
    // until the next breaking release per the SAML parser public-API contract.
    #[deprecated(
        since = "0.1.0",
        note = "use SamlParser::parse_verified() in production or SamlParser::parse_unverified() for tests"
    )]
    pub fn parse(xml: &str) -> Result<SamlAssertion, SamlError> {
        Self::parse_unverified(xml)
    }

    /// Parses an XML string containing a SAML 2.0 Assertion without signature verification.
    /// Explicitly intended for offline tests and diagnostics.
    pub fn parse_unverified(xml: &str) -> Result<SamlAssertion, SamlError> {
        Self::parse_internal(xml, None, true)
    }

    /// Parses an XML string containing a SAML 2.0 Assertion with mandatory cryptographic signature verification.
    pub fn parse_signed(
        xml: &str,
        verifier: &dyn SamlSignatureVerifier,
    ) -> Result<SamlAssertion, SamlError> {
        Self::parse_internal(xml, Some(verifier), false)
    }

    /// Parses an XML string containing a SAML 2.0 Assertion with mandatory cryptographic signature verification.
    ///
    /// Alias for [`SamlParser::parse_signed`]; kept so the verified entry point is
    /// discoverable under its descriptive name.
    pub fn parse_verified(
        xml: &str,
        verifier: &dyn SamlSignatureVerifier,
    ) -> Result<SamlAssertion, SamlError> {
        Self::parse_signed(xml, verifier)
    }

    /// Parses an XML string containing a SAML 2.0 Assertion using a namespace-aware XML parser
    /// and verifies digital signatures against the exact node consumed by the application.
    pub fn parse_with_verifier(
        xml: &str,
        verifier: Option<&dyn SamlSignatureVerifier>,
    ) -> Result<SamlAssertion, SamlError> {
        match verifier {
            Some(v) => Self::parse_internal(xml, Some(v), false),
            None => Err(SamlError::MissingSignatureVerifier),
        }
    }

    fn parse_internal(
        xml: &str,
        verifier: Option<&dyn SamlSignatureVerifier>,
        allow_unverified: bool,
    ) -> Result<SamlAssertion, SamlError> {
        let mut reader = NsReader::from_str(xml);
        reader.config_mut().trim_text(true);
        reader.config_mut().expand_empty_elements = true;

        let mut assertion_count = 0usize;
        let mut assertion_id: Option<String> = None;
        let mut response_id: Option<String> = None;
        let mut destination: Option<String> = None;

        let mut assertion_issuer: Option<String> = None;
        let mut response_issuer: Option<String> = None;
        let mut active_issuer_buf: Option<String> = None;
        let mut fields = AssertionFields::default();
        let mut not_before: Option<u64> = None;
        let mut not_on_or_after: Option<u64> = None;

        let mut assertion_start_offset: Option<usize> = None;
        let mut assertion_end_offset: Option<usize> = None;

        let mut signature_references: Vec<String> = Vec::new();

        let mut element_stack: Vec<(String, XmlNs)> = Vec::new();
        let mut in_assertion = false;
        let mut current_attr_name: Option<String> = None;

        // Why: in signed mode an empty `<ds:Reference>` does not bind to any signed
        // element and is a signature-verification bypass. Unverified diagnostic mode keeps
        // RFC 3275 leniency for root-document `URI=""` and Response-level envelopes.
        let signed_mode = verifier.is_some();

        loop {
            let event_start = reader.buffer_position() as usize;

            match reader.read_resolved_event() {
                Ok((res, Event::Start(ref e))) => {
                    let local = e.local_name();
                    let local_name = local.as_ref();
                    let is_saml_ns = is_ns(&res, ASSERTION);
                    let is_protocol_ns = is_ns(&res, PROTOCOL);
                    let is_dsig_ns = is_ns(&res, DSIG);
                    let ns = if is_saml_ns {
                        XmlNs::Assertion
                    } else if is_protocol_ns {
                        XmlNs::Protocol
                    } else {
                        XmlNs::Other
                    };

                    if local_name == "Assertion" {
                        if !is_saml_ns {
                            return Err(SamlError::SignatureWrappingDetected(
                                "Assertion element has invalid or foreign namespace URI".into(),
                            ));
                        }
                        assertion_count += 1;
                        if assertion_count > 1 {
                            return Err(SamlError::SignatureWrappingDetected(
                                "multiple Assertion elements detected; possible XML signature wrapping attack".into(),
                            ));
                        }
                        in_assertion = true;
                        assertion_start_offset = Some(event_start);

                        for attr in e.attributes().flatten() {
                            let local_k = attr.key.local_name();
                            if local_k.as_ref() == "ID" {
                                assertion_id = Some(attr.value.as_ref().to_string());
                            }
                        }
                    } else if local_name == "Response" && is_protocol_ns {
                        for attr in e.attributes().flatten() {
                            let local_k = attr.key.local_name();
                            let k = local_k.as_ref();
                            if k == "ID" {
                                response_id = Some(attr.value.as_ref().to_string());
                            } else if k == "Destination" {
                                destination = Some(attr.value.as_ref().to_string());
                            }
                        }
                    } else if local_name == "Reference" && is_dsig_ns {
                        for attr in e.attributes().flatten() {
                            let local_k = attr.key.local_name();
                            if local_k.as_ref() == "URI" {
                                let uri = attr.value.as_ref();
                                let ref_id = uri.strip_prefix('#').unwrap_or(uri);
                                signature_references.push(ref_id.to_string());
                            }
                        }
                    } else if local_name == "Issuer" && is_saml_ns {
                        // Why: anchor `<Issuer>` text accumulation to direct children of
                        // `<Assertion>` or `<Response>` so nested elements like `<Advice><Issuer>`
                        // cannot spoof the assertion issuer and XML comments cannot split/drop text.
                        if matches!(
                            element_stack
                                .last()
                                .map(|(name, parent_ns)| (name.as_str(), *parent_ns)),
                            Some(("Assertion", XmlNs::Assertion) | ("Response", XmlNs::Protocol))
                        ) {
                            active_issuer_buf = Some(String::new());
                        }
                    } else if in_assertion && is_saml_ns {
                        if local_name == "Conditions" {
                            for attr in e.attributes().flatten() {
                                let local_k = attr.key.local_name();
                                let k = local_k.as_ref();
                                if k == "NotBefore" {
                                    not_before = Some(parse_iso8601_strict(attr.value.as_ref())?);
                                } else if k == "NotOnOrAfter" {
                                    not_on_or_after =
                                        Some(parse_iso8601_strict(attr.value.as_ref())?);
                                }
                            }
                        } else if local_name == "Attribute" {
                            for attr in e.attributes().flatten() {
                                let local_k = attr.key.local_name();
                                if local_k.as_ref() == "Name" {
                                    current_attr_name = Some(attr.value.as_ref().to_string());
                                }
                            }
                        }
                    }

                    element_stack.push((local_name.to_string(), ns));
                }
                Ok((res, Event::End(ref e))) => {
                    let local = e.local_name();
                    let local_name = local.as_ref();
                    if local_name == "Issuer" && is_ns(&res, ASSERTION) {
                        if let Some(raw_issuer) = active_issuer_buf.take() {
                            let text = raw_issuer.trim().to_string();
                            if !text.is_empty() {
                                let grand_parent = element_stack
                                    .len()
                                    .checked_sub(2)
                                    .and_then(|idx| element_stack.get(idx))
                                    .map(|(name, ns)| (name.as_str(), *ns));
                                match grand_parent {
                                    Some(("Assertion", XmlNs::Assertion)) => {
                                        if let Some(prev) = &assertion_issuer {
                                            if prev != &text {
                                                return Err(SamlError::SignatureWrappingDetected(
                                                    "multiple conflicting Issuer elements detected in Assertion".into(),
                                                ));
                                            }
                                        } else {
                                            assertion_issuer = Some(text);
                                        }
                                    }
                                    Some(("Response", XmlNs::Protocol))
                                        if response_issuer.is_none() =>
                                    {
                                        response_issuer = Some(text);
                                    }
                                    _ => {}
                                }
                            }
                        }
                    } else if local_name == "Assertion" && is_ns(&res, ASSERTION) {
                        assertion_end_offset = Some(reader.buffer_position() as usize);
                        in_assertion = false;
                    } else if local_name == "Attribute" && is_ns(&res, ASSERTION) {
                        current_attr_name = None;
                    }
                    element_stack.pop();
                }
                Ok((_res, Event::Text(ref t))) => {
                    let chunk = quick_xml::escape::unescape(t.as_ref())
                        .map(|c| c.into_owned())
                        .unwrap_or_else(|_| t.as_ref().to_string());
                    record_assertion_text_chunk(
                        &chunk,
                        &element_stack,
                        in_assertion,
                        &mut active_issuer_buf,
                        current_attr_name.as_deref(),
                        &mut fields,
                    );
                }
                Ok((_res, Event::CData(ref c))) => {
                    record_assertion_text_chunk(
                        c.as_ref(),
                        &element_stack,
                        in_assertion,
                        &mut active_issuer_buf,
                        current_attr_name.as_deref(),
                        &mut fields,
                    );
                }
                Ok((_res, Event::Eof)) => break,
                Err(err) => {
                    return Err(SamlError::Malformed(format!("XML parser error: {err}")));
                }
                _ => {}
            }
        }

        if assertion_count == 0 {
            return Err(SamlError::Malformed(
                "missing Assertion element in SAML payload".into(),
            ));
        }

        // Validate signature references against assertion ID or response ID.
        // Why: an empty `<ds:Reference URI="">` binds to the whole document rather than a
        // signed element; it is tolerated only in unverified diagnostic mode and rejected
        // outright in signed mode as a signature-wrapping bypass.
        for ref_id in &signature_references {
            if ref_id.is_empty() {
                if signed_mode {
                    return Err(SamlError::SignatureWrappingDetected(
                        "empty ds:Reference URI in signed assertion does not bind to a signed element"
                            .into(),
                    ));
                }
                let matches_root_document = response_id.is_some() || assertion_id.is_some();
                if !matches_root_document {
                    return Err(SamlError::SignatureWrappingDetected(
                        "empty ds:Reference URI matches no signed element".into(),
                    ));
                }
                continue;
            }

            let matches_assertion = assertion_id.as_deref() == Some(ref_id.as_str());
            let matches_response = response_id.as_deref() == Some(ref_id.as_str());
            if !matches_assertion && !matches_response {
                let expected = match (&assertion_id, &response_id) {
                    (Some(a), Some(r)) => {
                        format!("assertion ID '{a}', response ID '{r}'")
                    }
                    (Some(a), None) => format!("assertion ID '{a}'"),
                    (None, Some(r)) => format!("response ID '{r}'"),
                    (None, None) => "known element ID".to_string(),
                };
                return Err(SamlError::SignatureWrappingDetected(format!(
                    "signature reference URI '#{ref_id}' does not match {expected}"
                )));
            }
        }

        // Why: in signed mode (`verifier.is_some()`), mandate non-empty Assertion/@ID and at least
        // one `<ds:Reference>` covering that Assertion/@ID so an assertion without a signature or
        // covered only by a Response-level reference cannot slip past signature verification.
        if signed_mode {
            let Some(bound_assertion_id) =
                assertion_id.as_deref().filter(|id| !id.trim().is_empty())
            else {
                return Err(SamlError::SignatureWrappingDetected(
                    "signed Assertion is missing a non-empty ID attribute".into(),
                ));
            };
            if signature_references.is_empty() {
                return Err(SamlError::SignatureWrappingDetected(
                    "signed SAML payload is missing ds:Signature/ds:SignedInfo/ds:Reference".into(),
                ));
            }
            if !signature_references
                .iter()
                .any(|ref_id| ref_id == bound_assertion_id)
            {
                return Err(SamlError::SignatureWrappingDetected(format!(
                    "no ds:Reference URI covers signed assertion ID '{bound_assertion_id}'"
                )));
            }
        }

        let issuer = assertion_issuer
            .or(response_issuer)
            .ok_or_else(|| SamlError::Malformed("missing Issuer element".into()))?;
        if issuer.is_empty() {
            return Err(SamlError::Malformed("missing Issuer element".into()));
        }

        let subject = fields
            .subject
            .ok_or_else(|| SamlError::Malformed("missing NameID in Subject".into()))?;
        if subject.is_empty() {
            return Err(SamlError::Malformed("missing NameID in Subject".into()));
        }

        let id = assertion_id.unwrap_or_else(|| "assertion".to_string());

        let assertion_raw_slice = match (assertion_start_offset, assertion_end_offset) {
            (Some(start), Some(end)) if start < end && end <= xml.len() => &xml[start..end],
            _ => xml,
        };

        if let Some(v) = verifier {
            v.verify_assertion_signature(xml, &id, assertion_raw_slice)?;
        } else if !allow_unverified {
            return Err(SamlError::MissingSignatureVerifier);
        }

        Ok(SamlAssertion {
            id,
            issuer,
            subject,
            audience: fields.audience,
            destination,
            not_before,
            not_on_or_after,
            attributes: fields.attributes,
        })
    }

    /// Validates time conditions and audience restrictions against the assertion.
    pub fn validate(
        assertion: &SamlAssertion,
        current_epoch_sec: u64,
        expected_audience: Option<&str>,
    ) -> Result<(), SamlError> {
        if let Some(not_before) = assertion.not_before
            && current_epoch_sec < not_before
        {
            return Err(SamlError::NotYetValid {
                not_before,
                current: current_epoch_sec,
            });
        }

        let not_on_or_after = assertion
            .not_on_or_after
            .ok_or(SamlError::MissingExpirationCondition)?;

        if current_epoch_sec >= not_on_or_after {
            return Err(SamlError::Expired {
                not_on_or_after,
                current: current_epoch_sec,
            });
        }

        if let Some(expected) = expected_audience {
            match &assertion.audience {
                Some(received) if received == expected => {}
                Some(received) => {
                    return Err(SamlError::AudienceMismatch {
                        expected: expected.to_string(),
                        received: received.clone(),
                    });
                }
                None => {
                    return Err(SamlError::AudienceMismatch {
                        expected: expected.to_string(),
                        received: "<none>".to_string(),
                    });
                }
            }
        }

        Ok(())
    }

    /// Strictly validates time conditions, audience, issuer, and destination against the assertion.
    pub fn validate_strict(
        assertion: &SamlAssertion,
        current_epoch_sec: u64,
        expected_audience: Option<&str>,
        expected_issuer: Option<&str>,
        expected_destination: Option<&str>,
    ) -> Result<(), SamlError> {
        Self::validate(assertion, current_epoch_sec, expected_audience)?;

        if let Some(expected) = expected_issuer
            && assertion.issuer != expected
        {
            return Err(SamlError::IssuerMismatch {
                expected: expected.to_string(),
                received: assertion.issuer.clone(),
            });
        }

        if let Some(expected) = expected_destination {
            match &assertion.destination {
                Some(received) if received == expected => {}
                Some(received) => {
                    return Err(SamlError::DestinationMismatch {
                        expected: expected.to_string(),
                        received: received.clone(),
                    });
                }
                None => {
                    return Err(SamlError::DestinationMismatch {
                        expected: expected.to_string(),
                        received: "<none>".to_string(),
                    });
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod saml_parser_and_iso8601_proptests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn parse_unverified_never_panics_on_arbitrary_utf8(xml in "\\PC*") {
            let _ = SamlParser::parse_unverified(&xml);
            let _ = SamlParser::parse_entity_id(&xml);
        }

        #[test]
        fn parse_iso8601_strict_never_panics_on_arbitrary_or_multibyte_offsets(
            s in "\\PC*",
            suffix in "\\PC*",
        ) {
            let _ = parse_iso8601_strict(&s);
            let combined = format!("2026-11-19T12:00:00{suffix}");
            let _ = parse_iso8601_strict(&combined);
        }

        #[test]
        fn parse_iso8601_strict_converts_valid_rfc3339_instants_and_rejects_pre_epoch(
            year in 1970i32..=2099,
            month in 1u32..=12,
            day in 1u32..=28,
            hour in 0u32..=23,
            minute in 0u32..=59,
            second in 0u32..=59,
            use_z in any::<bool>(),
            positive_offset in any::<bool>(),
            off_h in 0u32..=23,
            off_m in 0u32..=59,
        ) {
            let naive = chrono::NaiveDate::from_ymd_opt(year, month, day)
                .unwrap()
                .and_hms_opt(hour, minute, second)
                .unwrap();

            let (tz_suffix, offset_secs) = if use_z {
                ("Z".to_string(), 0i64)
            } else {
                let sign = if positive_offset { '+' } else { '-' };
                let secs = (off_h as i64) * 3600 + (off_m as i64) * 60;
                let signed = if positive_offset { secs } else { -secs };
                (format!("{sign}{off_h:02}:{off_m:02}"), signed)
            };

            let ts = format!(
                "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}{tz_suffix}"
            );
            let expected_utc = naive.and_utc().timestamp() - offset_secs;

            match expected_utc {
                secs if secs >= 0 => {
                    let parsed = parse_iso8601_strict(&ts)
                        .expect("valid post-epoch RFC 3339 timestamp must succeed");
                    prop_assert_eq!(parsed, secs as u64);
                }
                _ => {
                    let err = parse_iso8601_strict(&ts)
                        .expect_err("pre-1970 UTC instant must fail closed");
                    prop_assert!(matches!(err, SamlError::Malformed(_)));
                }
            }
        }

        #[test]
        fn parse_iso8601_strict_rejects_out_of_range_offsets_space_separator_and_pre_1970(
            bad_hour in 24u32..=99,
            bad_min in 60u32..=99,
            pre_year in 1900i32..=1969,
            sign in prop::sample::select(vec!['+', '-']),
        ) {
            let bad_offset_h = format!("2026-11-19T12:00:00{sign}{bad_hour:02}:00");
            prop_assert!(matches!(
                parse_iso8601_strict(&bad_offset_h),
                Err(SamlError::Malformed(_))
            ));

            let bad_offset_m = format!("2026-11-19T12:00:00{sign}00:{bad_min:02}");
            prop_assert!(matches!(
                parse_iso8601_strict(&bad_offset_m),
                Err(SamlError::Malformed(_))
            ));

            prop_assert!(matches!(
                parse_iso8601_strict("2026-11-19 12:00:00Z"),
                Err(SamlError::Malformed(_))
            ));

            let pre_epoch = format!("{pre_year:04}-06-15T12:00:00Z");
            prop_assert!(matches!(
                parse_iso8601_strict(&pre_epoch),
                Err(SamlError::Malformed(_))
            ));
        }
    }
}
