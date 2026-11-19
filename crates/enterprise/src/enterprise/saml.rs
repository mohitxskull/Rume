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
    Dsig,
    Other,
}

#[derive(Default)]
struct AssertionFields {
    subject: Option<String>,
    audience: Option<String>,
    attributes: BTreeMap<String, Vec<String>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TextTarget {
    AssertionIssuer,
    ResponseIssuer,
    SubjectNameId,
    Audience,
    AttributeValue,
}

struct ActiveTextCapture {
    target: TextTarget,
    depth: usize,
    buf: String,
    /// Owning `<Attribute Name="...">` captured when an `AttributeValue` capture opens.
    ///
    /// Why: SAML permits structured XML inside `<AttributeValue>`, so a nested `<Attribute>`
    /// start/end must not change or clear the name the buffered text belongs to; the capture
    /// carries its own copy instead of reading the parser-wide `current_attr_name` at close.
    attr_name: Option<String>,
}

fn resolve_general_ref(raw_ref: &quick_xml::events::BytesRef<'_>) -> Result<String, SamlError> {
    if let Some(ch) = raw_ref.resolve_char_ref().map_err(|err| {
        SamlError::Malformed(format!("invalid numeric XML character reference: {err}"))
    })? {
        return Ok(ch.to_string());
    }
    let name = raw_ref.as_ref();
    if let Some(resolved) = quick_xml::escape::resolve_xml_entity(name) {
        return Ok(resolved.to_string());
    }
    Err(SamlError::Malformed(format!(
        "unresolved XML entity reference '&{name};'"
    )))
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
        // Why: keep internal whitespace intact across text, CDATA, comment, and entity-ref
        // boundaries; outer whitespace is trimmed once when each captured claim element closes.
        reader.config_mut().trim_text(false);
        reader.config_mut().expand_empty_elements = true;

        let mut assertion_count = 0usize;
        let mut assertion_id: Option<String> = None;
        let mut response_id: Option<String> = None;
        let mut destination: Option<String> = None;

        let mut assertion_issuer: Option<String> = None;
        let mut response_issuer: Option<String> = None;
        let mut active_capture: Option<ActiveTextCapture> = None;
        let mut fields = AssertionFields::default();
        let mut not_before: Option<u64> = None;
        let mut not_on_or_after: Option<u64> = None;

        let mut assertion_start_offset: Option<usize> = None;
        let mut assertion_end_offset: Option<usize> = None;

        let mut signature_references: Vec<String> = Vec::new();

        let mut element_stack: Vec<(String, XmlNs)> = Vec::new();
        let mut in_assertion = false;
        let mut current_attr_name: Option<String> = None;
        let mut current_attr_depth: usize = 0;

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
                    } else if is_dsig_ns {
                        XmlNs::Dsig
                    } else {
                        XmlNs::Other
                    };
                    let new_depth = element_stack.len() + 1;
                    let parent = element_stack.last().map(|(n, pns)| (n.as_str(), *pns));

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
                        // Why: only `<ds:Reference>` elements directly under
                        // `<ds:Signature>/<ds:SignedInfo>` belong to a signature's signed
                        // info block; stray `<ds:Reference>` nodes elsewhere in the document
                        // must not satisfy the signed-mode structural gate.
                        let in_signed_info = element_stack.len() >= 2
                            && matches!(
                                (
                                    element_stack[element_stack.len() - 2].0.as_str(),
                                    element_stack[element_stack.len() - 2].1,
                                    element_stack[element_stack.len() - 1].0.as_str(),
                                    element_stack[element_stack.len() - 1].1,
                                ),
                                ("Signature", XmlNs::Dsig, "SignedInfo", XmlNs::Dsig)
                            );
                        if in_signed_info {
                            for attr in e.attributes().flatten() {
                                let local_k = attr.key.local_name();
                                if local_k.as_ref() == "URI" {
                                    let uri = attr.value.as_ref();
                                    let ref_id = uri.strip_prefix('#').unwrap_or(uri);
                                    signature_references.push(ref_id.to_string());
                                }
                            }
                        }
                    } else if local_name == "Issuer" && is_saml_ns && active_capture.is_none() {
                        // Why: anchor `<Issuer>` text accumulation to direct children of
                        // `<Assertion>` or `<Response>` and bind the capture to its opening
                        // stack depth so nested markup or comments cannot corrupt or drop it.
                        match parent {
                            Some(("Assertion", XmlNs::Assertion)) => {
                                active_capture = Some(ActiveTextCapture {
                                    target: TextTarget::AssertionIssuer,
                                    depth: new_depth,
                                    buf: String::new(),
                                    attr_name: None,
                                });
                            }
                            Some(("Response", XmlNs::Protocol)) => {
                                active_capture = Some(ActiveTextCapture {
                                    target: TextTarget::ResponseIssuer,
                                    depth: new_depth,
                                    buf: String::new(),
                                    attr_name: None,
                                });
                            }
                            _ => {}
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
                            // Why: only an `<Attribute>` outside any open text capture may set
                            // the attribute name; a nested `<Attribute>` inside an
                            // `<AttributeValue>` is structured value content and must not
                            // replace (or later clear) the owning attribute's name.
                            if active_capture.is_none() {
                                for attr in e.attributes().flatten() {
                                    let local_k = attr.key.local_name();
                                    if local_k.as_ref() == "Name" {
                                        current_attr_name = Some(attr.value.as_ref().to_string());
                                        current_attr_depth = new_depth;
                                    }
                                }
                            }
                        } else if active_capture.is_none() {
                            if local_name == "NameID"
                                && fields.subject.is_none()
                                && matches!(parent, Some(("Subject", XmlNs::Assertion)))
                            {
                                active_capture = Some(ActiveTextCapture {
                                    target: TextTarget::SubjectNameId,
                                    depth: new_depth,
                                    buf: String::new(),
                                    attr_name: None,
                                });
                            } else if local_name == "Audience" && fields.audience.is_none() {
                                active_capture = Some(ActiveTextCapture {
                                    target: TextTarget::Audience,
                                    depth: new_depth,
                                    buf: String::new(),
                                    attr_name: None,
                                });
                            } else if local_name == "AttributeValue" && current_attr_name.is_some()
                            {
                                active_capture = Some(ActiveTextCapture {
                                    target: TextTarget::AttributeValue,
                                    depth: new_depth,
                                    buf: String::new(),
                                    attr_name: current_attr_name.clone(),
                                });
                            }
                        }
                    }

                    element_stack.push((local_name.to_string(), ns));
                }
                Ok((res, Event::End(ref e))) => {
                    let local = e.local_name();
                    let local_name = local.as_ref();
                    if active_capture
                        .as_ref()
                        .is_some_and(|cap| cap.depth == element_stack.len())
                        && let Some(capture) = active_capture.take()
                    {
                        let text = capture.buf.trim().to_string();
                        if !text.is_empty() {
                            match capture.target {
                                TextTarget::AssertionIssuer => {
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
                                TextTarget::ResponseIssuer => {
                                    if response_issuer.is_none() {
                                        response_issuer = Some(text);
                                    }
                                }
                                TextTarget::SubjectNameId => {
                                    if fields.subject.is_none() {
                                        fields.subject = Some(text);
                                    }
                                }
                                TextTarget::Audience => {
                                    if fields.audience.is_none() {
                                        fields.audience = Some(text);
                                    }
                                }
                                TextTarget::AttributeValue => {
                                    if let Some(attr_name) = capture.attr_name {
                                        fields.attributes.entry(attr_name).or_default().push(text);
                                    }
                                }
                            }
                        }
                    }

                    if local_name == "Assertion" && is_ns(&res, ASSERTION) {
                        assertion_end_offset = Some(reader.buffer_position() as usize);
                        in_assertion = false;
                    } else if local_name == "Attribute"
                        && is_ns(&res, ASSERTION)
                        && element_stack.len() == current_attr_depth
                    {
                        // Why: clear only when the owning `<Attribute>` itself closes, not a
                        // nested one (see the matching Start-side guard).
                        current_attr_name = None;
                    }
                    element_stack.pop();
                }
                Ok((_res, Event::Text(ref t))) => {
                    if let Some(capture) = active_capture.as_mut()
                        && capture.depth == element_stack.len()
                    {
                        capture.buf.push_str(t.as_ref());
                    }
                }
                Ok((_res, Event::CData(ref c))) => {
                    if let Some(capture) = active_capture.as_mut()
                        && capture.depth == element_stack.len()
                    {
                        capture.buf.push_str(c.as_ref());
                    }
                }
                Ok((_res, Event::GeneralRef(ref r))) => {
                    let resolved = resolve_general_ref(r)?;
                    if let Some(capture) = active_capture.as_mut()
                        && capture.depth == element_stack.len()
                    {
                        capture.buf.push_str(&resolved);
                    }
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

    #[test]
    fn signed_mode_rejects_reference_outside_signature_signed_info() {
        use std::sync::atomic::{AtomicBool, Ordering};

        struct PanicIfCalled(AtomicBool);
        impl SamlSignatureVerifier for PanicIfCalled {
            fn verify_signature(&self, _: &str) -> Result<(), SamlError> {
                self.0.store(true, Ordering::SeqCst);
                Ok(())
            }
            fn verify_assertion_signature(
                &self,
                _: &str,
                _: &str,
                _: &str,
            ) -> Result<(), SamlError> {
                self.0.store(true, Ordering::SeqCst);
                Ok(())
            }
        }

        // Stray `<ds:Reference URI="#_a1"/>` outside `<ds:Signature><ds:SignedInfo>`.
        let xml = r##"<saml:Assertion xmlns:saml="urn:oasis:names:tc:SAML:2.0:assertion" xmlns:ds="http://www.w3.org/2000/09/xmldsig#" ID="_a1">
            <saml:Issuer>https://idp.example.com</saml:Issuer>
            <ds:Reference URI="#_a1"/>
            <saml:Subject><saml:NameID>alice@example.org</saml:NameID></saml:Subject>
        </saml:Assertion>"##;

        let verifier = PanicIfCalled(AtomicBool::new(false));
        let err = SamlParser::parse_signed(xml, &verifier)
            .expect_err("stray ds:Reference outside SignedInfo must be rejected");
        assert!(matches!(err, SamlError::SignatureWrappingDetected(_)));
        assert!(
            !verifier.0.load(Ordering::SeqCst),
            "verifier must not be invoked when ds:Reference lacks ds:Signature/ds:SignedInfo ancestry"
        );
    }

    #[test]
    fn claims_assemble_across_cdata_comments_entities_and_nested_issuer() {
        let xml = r#"<saml:Assertion xmlns:saml="urn:oasis:names:tc:SAML:2.0:assertion" ID="_a2">
            <saml:Issuer>https://idp.example.com<saml:Issuer>nested</saml:Issuer>/sso</saml:Issuer>
            <saml:Subject>
                <saml:NameID>alice<![CDATA[@example.org]]></saml:NameID>
            </saml:Subject>
            <saml:Conditions>
                <saml:AudienceRestriction>
                    <saml:Audience>https://sp.example.com/a&amp;b&#63;v=1</saml:Audience>
                </saml:AudienceRestriction>
            </saml:Conditions>
            <saml:AttributeStatement>
                <saml:Attribute Name="roles">
                    <saml:AttributeValue>team <!-- split --> lead &amp; ops</saml:AttributeValue>
                    <saml:AttributeValue>auditor</saml:AttributeValue>
                </saml:Attribute>
            </saml:AttributeStatement>
        </saml:Assertion>"#;

        let assertion = SamlParser::parse_unverified(xml).expect("valid assertion must parse");
        assert_eq!(assertion.issuer, "https://idp.example.com/sso");
        assert_eq!(assertion.subject, "alice@example.org");
        assert_eq!(
            assertion.audience.as_deref(),
            Some("https://sp.example.com/a&b?v=1")
        );
        assert_eq!(
            assertion.attributes.get("roles"),
            Some(&vec!["team  lead & ops".to_string(), "auditor".to_string()])
        );
    }

    #[test]
    fn unresolved_xml_entity_reference_fails_closed() {
        let xml = r#"<saml:Assertion xmlns:saml="urn:oasis:names:tc:SAML:2.0:assertion" ID="_a3">
            <saml:Issuer>https://idp.example.com</saml:Issuer>
            <saml:Subject><saml:NameID>alice&unknown;example.org</saml:NameID></saml:Subject>
        </saml:Assertion>"#;

        let err = SamlParser::parse_unverified(xml)
            .expect_err("unresolved XML entity reference must fail closed");
        assert!(matches!(err, SamlError::Malformed(_)));
    }

    #[test]
    fn nested_attribute_inside_attribute_value_keeps_owning_attribute_name() {
        let xml = r#"<saml:Assertion xmlns:saml="urn:oasis:names:tc:SAML:2.0:assertion" ID="_a4">
            <saml:Issuer>https://idp.example.com</saml:Issuer>
            <saml:Subject>
                <saml:NameID>alice@example.org</saml:NameID>
            </saml:Subject>
            <saml:AttributeStatement>
                <saml:Attribute Name="roles">
                    <saml:AttributeValue>user<saml:Attribute Name="meta"/></saml:AttributeValue>
                    <saml:AttributeValue>admin</saml:AttributeValue>
                </saml:Attribute>
            </saml:AttributeStatement>
        </saml:Assertion>"#;

        let assertion = SamlParser::parse_unverified(xml).expect("valid assertion must parse");
        assert_eq!(
            assertion.attributes.get("roles"),
            Some(&vec!["user".to_string(), "admin".to_string()]),
            "nested <Attribute> must neither rename nor clear the owning attribute"
        );
        assert!(!assertion.attributes.contains_key("meta"));
    }
}
