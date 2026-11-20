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
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Mutex;
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
    audiences: Vec<String>,
    audience_restrictions: Vec<Vec<String>>,
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

    /// SubjectConfirmationData Recipient mismatch.
    #[error("recipient mismatch: expected '{expected}', received '{received}'")]
    RecipientMismatch { expected: String, received: String },

    /// InResponseTo correlation ID mismatch.
    #[error("InResponseTo mismatch: expected '{expected}', received '{received}'")]
    InResponseToMismatch { expected: String, received: String },

    /// Assertion ID was already consumed (replay detected).
    #[error("SAML assertion replay detected for ID '{0}'")]
    AssertionReplayed(String),
}

/// Replay-protection cache for consumed SAML assertion IDs.
pub trait SamlReplayCache: Send + Sync {
    /// Records `assertion_id` with expiration `expires_at` (epoch seconds).
    /// Returns `Ok(true)` if fresh (newly recorded), or `Ok(false)` if already seen.
    fn record_if_absent(&self, assertion_id: &str, expires_at: u64) -> Result<bool, SamlError>;
}

/// Thread-safe in-memory [`SamlReplayCache`] suitable for single-instance deployments and tests.
#[derive(Debug, Default)]
pub struct InMemorySamlReplayCache {
    seen: Mutex<HashMap<String, u64>>,
}

impl InMemorySamlReplayCache {
    /// Creates a new empty replay cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// Evicts entries whose expiration is `<= now_epoch_sec`.
    pub fn prune_expired(&self, now_epoch_sec: u64) {
        if let Ok(mut guard) = self.seen.lock() {
            guard.retain(|_, exp| *exp > now_epoch_sec);
        }
    }
}

impl SamlReplayCache for InMemorySamlReplayCache {
    fn record_if_absent(&self, assertion_id: &str, expires_at: u64) -> Result<bool, SamlError> {
        let mut guard = self
            .seen
            .lock()
            .map_err(|e| SamlError::Malformed(format!("replay cache lock poisoned: {e}")))?;
        if guard.contains_key(assertion_id) {
            return Ok(false);
        }
        guard.insert(assertion_id.to_string(), expires_at);
        Ok(true)
    }
}

/// Parsed `<saml:SubjectConfirmationData>` attributes (SAML 2.0 Core §2.4.1.2).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SamlSubjectConfirmationData {
    /// ACS recipient URL (`Recipient` attribute).
    pub recipient: Option<String>,
    /// Confirmation expiration timestamp (`NotOnOrAfter` attribute, epoch seconds).
    pub not_on_or_after: Option<u64>,
    /// Correlated AuthnRequest ID (`InResponseTo` attribute).
    pub in_response_to: Option<String>,
}

/// Configuration for full SAML 2.0 Web Browser SSO assertion validation.
pub struct SamlValidationConfig<'a> {
    /// Expected service provider audience URI.
    pub expected_audience: Option<&'a str>,
    /// Expected IdP issuer URI.
    pub expected_issuer: Option<&'a str>,
    /// Expected Response `@Destination` URI.
    pub expected_destination: Option<&'a str>,
    /// Expected `SubjectConfirmationData/@Recipient` ACS URL.
    pub expected_recipient: Option<&'a str>,
    /// Expected `InResponseTo` AuthnRequest ID (checked against both signed
    /// `SubjectConfirmationData/@InResponseTo` and outer `Response/@InResponseTo`).
    pub expected_in_response_to: Option<&'a str>,
    /// Allowed clock skew in seconds for `NotBefore` / `NotOnOrAfter` checks (default: `0`).
    pub clock_skew_secs: u64,
    /// Optional replay-protection cache rejecting reused assertion IDs.
    pub replay_cache: Option<&'a dyn SamlReplayCache>,
    /// Whether at least one valid bearer `<SubjectConfirmation>` is required (default: `true`).
    ///
    /// Why: SAML 2.0 Web Browser SSO profile requires a valid bearer confirmation with
    /// `NotOnOrAfter`; callers performing partial assertion checks (`validate`/`validate_strict`)
    /// opt out explicitly via `require_bearer_confirmation: false`.
    pub require_bearer_confirmation: bool,
}

impl Default for SamlValidationConfig<'_> {
    fn default() -> Self {
        Self {
            expected_audience: None,
            expected_issuer: None,
            expected_destination: None,
            expected_recipient: None,
            expected_in_response_to: None,
            clock_skew_secs: 0,
            replay_cache: None,
            require_bearer_confirmation: true,
        }
    }
}

/// Interface for verifying cryptographic signatures on SAML assertions.
pub trait SamlSignatureVerifier: Send + Sync {
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
    /// Flattened audience URIs across all `<AudienceRestriction>` elements.
    pub audiences: Vec<String>,
    /// Grouped `<AudienceRestriction>` elements (SAML 2.0 Core §2.5.1.4:
    /// OR within each restriction group, AND across all restriction groups).
    pub audience_restrictions: Vec<Vec<String>>,
    /// Destination endpoint URI.
    pub destination: Option<String>,
    /// Whether the assertion was parsed inside a `<samlp:Response>` envelope.
    pub has_response_envelope: bool,
    /// Outer `<samlp:Response InResponseTo="...">` attribute, if present.
    pub response_in_response_to: Option<String>,
    /// Parsed bearer `<saml:SubjectConfirmationData>` alternatives (SAML 2.0 Web Browser SSO
    /// permits multiple bearer confirmations and succeeds if at least one satisfies all checks).
    pub subject_confirmations: Vec<SamlSubjectConfirmationData>,
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
                        for attr_res in e.attributes() {
                            let attr = attr_res.map_err(|err| {
                                SamlError::Malformed(format!("malformed XML attribute: {err}"))
                            })?;
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
        let mut has_response_envelope = false;
        let mut response_in_response_to: Option<String> = None;
        let mut subject_confirmations: Vec<SamlSubjectConfirmationData> = Vec::new();
        let mut current_sc_is_bearer: Option<bool> = None;
        let mut current_audience_group: Option<usize> = None;
        let mut seen_ids: BTreeSet<String> = BTreeSet::new();

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
                    let grandparent = element_stack
                        .len()
                        .checked_sub(2)
                        .and_then(|idx| element_stack.get(idx))
                        .map(|(n, pns)| (n.as_str(), *pns));
                    let great_grandparent = element_stack
                        .len()
                        .checked_sub(3)
                        .and_then(|idx| element_stack.get(idx))
                        .map(|(n, pns)| (n.as_str(), *pns));
                    // Why: XML-DSig enveloped-signature transforms strip the entire `<ds:Signature>`
                    // subtree before computing the digest. An attacker who holds any validly signed
                    // assertion can inject `<saml:Subject>`, `<saml:Conditions>`, `<saml:Audience>`,
                    // or `<saml:Attribute>` inside `<ds:Signature>` (e.g. `<ds:Object>` or
                    // `<ds:KeyInfo>`) without invalidating the signature. Never extract any SAML
                    // claims while a `<ds:Signature>` element is on the ancestor stack.
                    let in_signature = element_stack
                        .iter()
                        .any(|(n, el_ns)| n == "Signature" && *el_ns == XmlNs::Dsig);

                    const XSI_NAMESPACE: &str = "http://www.w3.org/2001/XMLSchema-instance";

                    // Why: parse every attribute strictly (failing closed on malformed
                    // attributes instead of `.flatten()`), reject duplicate `ID`
                    // attributes anywhere in the document to defeat XSW ID-collision
                    // attacks, and resolve `xsi:type` by both attribute namespace (`XSI`)
                    // and QName prefix against the element's in-scope namespace map.
                    let mut parsed_attrs: Vec<(String, String)> = Vec::new();
                    let mut xsi_type_raw: Option<String> = None;
                    let mut xsi_type_is_saml_audience_restriction = false;
                    for attr_res in e.attributes() {
                        let attr = attr_res.map_err(|err| {
                            SamlError::Malformed(format!("malformed XML attribute: {err}"))
                        })?;
                        let (attr_ns, attr_local) = reader.resolver().resolve_attribute(attr.key);
                        let k = attr_local.as_ref().to_string();
                        // Why: `Attribute.value` retains raw XML character/entity escapes
                        // (e.g. `&amp;` in URLs like `Recipient="https://sp/acs?a=1&amp;b=2"`).
                        // `normalized_value()` decodes escapes and fails closed on invalid refs.
                        let v = attr
                            .normalized_value(quick_xml::XmlVersion::Implicit1_0)
                            .map_err(|err| {
                                SamlError::Malformed(format!(
                                    "malformed XML attribute value: {err}"
                                ))
                            })?
                            .into_owned();
                        if is_ns(&attr_ns, XSI_NAMESPACE) && k == "type" {
                            let (type_ns, type_local) = reader
                                .resolver()
                                .resolve_element(quick_xml::name::QName(&v));
                            xsi_type_is_saml_audience_restriction = is_ns(&type_ns, ASSERTION)
                                && type_local.as_ref() == "AudienceRestrictionType";
                            xsi_type_raw = Some(v.clone());
                        }
                        if k == "ID" && !v.is_empty() && !seen_ids.insert(v.clone()) {
                            return Err(SamlError::SignatureWrappingDetected(format!(
                                "duplicate ID attribute '{v}' detected; possible XML signature wrapping attack"
                            )));
                        }
                        parsed_attrs.push((k, v));
                    }

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

                        for (k, v) in &parsed_attrs {
                            if k == "ID" {
                                assertion_id = Some(v.clone());
                            }
                        }
                    } else if local_name == "Response" && is_protocol_ns {
                        has_response_envelope = true;
                        for (k, v) in &parsed_attrs {
                            if k == "ID" {
                                response_id = Some(v.clone());
                            } else if k == "Destination" {
                                destination = Some(v.clone());
                            } else if k == "InResponseTo" {
                                response_in_response_to = Some(v.clone());
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
                            for (k, v) in &parsed_attrs {
                                if k == "URI" {
                                    let ref_id = v.strip_prefix('#').unwrap_or(v);
                                    signature_references.push(ref_id.to_string());
                                }
                            }
                        }
                    } else if !in_signature
                        && local_name == "Issuer"
                        && is_saml_ns
                        && active_capture.is_none()
                    {
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
                    } else if in_assertion && !in_signature && is_saml_ns {
                        // Why: in addition to excluding `<ds:Signature>` subtrees, anchor SAML
                        // claims (`Conditions`, `Subject`/`NameID`, `SubjectConfirmationData`,
                        // `Audience`, `Attribute`, `AttributeValue`) to their schema positions
                        // under `<Assertion>` so extension containers such as `<Advice>` cannot
                        // inject forged claims.
                        if local_name == "Conditions"
                            && matches!(parent, Some(("Assertion", XmlNs::Assertion)))
                        {
                            for (k, v) in &parsed_attrs {
                                if k == "NotBefore" {
                                    not_before = Some(parse_iso8601_strict(v)?);
                                } else if k == "NotOnOrAfter" {
                                    not_on_or_after = Some(parse_iso8601_strict(v)?);
                                }
                            }
                        } else if local_name == "AudienceRestriction"
                            && matches!(parent, Some(("Conditions", XmlNs::Assertion)))
                            && matches!(grandparent, Some(("Assertion", XmlNs::Assertion)))
                        {
                            fields.audience_restrictions.push(Vec::new());
                            current_audience_group = Some(fields.audience_restrictions.len() - 1);
                        } else if local_name == "Condition"
                            && matches!(parent, Some(("Conditions", XmlNs::Assertion)))
                            && matches!(grandparent, Some(("Assertion", XmlNs::Assertion)))
                        {
                            // Why: SAML Core §2.5.1 allows `<Condition xsi:type="...:AudienceRestrictionType">`
                            // as an equivalent audience restriction ONLY when `xsi:type` is in the
                            // XMLSchema-instance namespace AND its QName resolves to `AudienceRestrictionType`
                            // in the SAML 2.0 assertion namespace; any other condition must fail closed.
                            if xsi_type_is_saml_audience_restriction {
                                fields.audience_restrictions.push(Vec::new());
                                current_audience_group =
                                    Some(fields.audience_restrictions.len() - 1);
                            } else {
                                return Err(SamlError::Malformed(format!(
                                    "unsupported SAML Condition element (xsi:type={xsi_type_raw:?})"
                                )));
                            }
                        } else if local_name == "SubjectConfirmation"
                            && matches!(parent, Some(("Subject", XmlNs::Assertion)))
                            && matches!(grandparent, Some(("Assertion", XmlNs::Assertion)))
                        {
                            let method = parsed_attrs
                                .iter()
                                .find(|(k, _)| k == "Method")
                                .map(|(_, v)| v.as_str());
                            // Why: SAML 2.0 Core §2.4.1.1 requires an explicit `Method` attribute;
                            // Web Browser SSO only admits `urn:oasis:names:tc:SAML:2.0:cm:bearer`.
                            current_sc_is_bearer =
                                Some(method == Some("urn:oasis:names:tc:SAML:2.0:cm:bearer"));
                        } else if local_name == "SubjectConfirmationData"
                            && matches!(parent, Some(("SubjectConfirmation", XmlNs::Assertion)))
                            && matches!(grandparent, Some(("Subject", XmlNs::Assertion)))
                            && matches!(great_grandparent, Some(("Assertion", XmlNs::Assertion)))
                        {
                            if current_sc_is_bearer == Some(true) {
                                let mut scd = SamlSubjectConfirmationData::default();
                                for (k, v) in &parsed_attrs {
                                    if k == "Recipient" {
                                        scd.recipient = Some(v.clone());
                                    } else if k == "NotOnOrAfter" {
                                        scd.not_on_or_after = Some(parse_iso8601_strict(v)?);
                                    } else if k == "InResponseTo" {
                                        scd.in_response_to = Some(v.clone());
                                    }
                                }
                                subject_confirmations.push(scd);
                            }
                        } else if local_name == "Attribute"
                            && matches!(parent, Some(("AttributeStatement", XmlNs::Assertion)))
                            && matches!(grandparent, Some(("Assertion", XmlNs::Assertion)))
                        {
                            // Why: only an `<Attribute>` directly under `<Assertion>/<AttributeStatement>`
                            // and outside any open text capture may set the attribute name; a nested
                            // `<Attribute>` inside an `<AttributeValue>` or `<ds:Signature>` must not
                            // replace (or later clear) the owning attribute's name.
                            if active_capture.is_none() {
                                for (k, v) in &parsed_attrs {
                                    if k == "Name" {
                                        current_attr_name = Some(v.clone());
                                        current_attr_depth = new_depth;
                                    }
                                }
                            }
                        } else if active_capture.is_none() {
                            if local_name == "NameID"
                                && fields.subject.is_none()
                                && matches!(parent, Some(("Subject", XmlNs::Assertion)))
                                && matches!(grandparent, Some(("Assertion", XmlNs::Assertion)))
                            {
                                active_capture = Some(ActiveTextCapture {
                                    target: TextTarget::SubjectNameId,
                                    depth: new_depth,
                                    buf: String::new(),
                                    attr_name: None,
                                });
                            } else if local_name == "Audience"
                                && ((matches!(
                                    parent,
                                    Some(("AudienceRestriction" | "Condition", XmlNs::Assertion))
                                ) && matches!(
                                    grandparent,
                                    Some(("Conditions", XmlNs::Assertion))
                                ) && matches!(
                                    great_grandparent,
                                    Some(("Assertion", XmlNs::Assertion))
                                )) || (matches!(
                                    parent,
                                    Some(("Conditions", XmlNs::Assertion))
                                ) && matches!(
                                    grandparent,
                                    Some(("Assertion", XmlNs::Assertion))
                                )))
                            {
                                active_capture = Some(ActiveTextCapture {
                                    target: TextTarget::Audience,
                                    depth: new_depth,
                                    buf: String::new(),
                                    attr_name: None,
                                });
                            } else if local_name == "AttributeValue"
                                && current_attr_name.is_some()
                                && matches!(parent, Some(("Attribute", XmlNs::Assertion)))
                                && element_stack.len() == current_attr_depth
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
                                    if let Some(group_idx) = current_audience_group
                                        && let Some(group) =
                                            fields.audience_restrictions.get_mut(group_idx)
                                    {
                                        group.push(text.clone());
                                    } else {
                                        fields.audience_restrictions.push(vec![text.clone()]);
                                    }
                                    fields.audiences.push(text);
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
                    } else if matches!(local_name, "AudienceRestriction" | "Condition")
                        && is_ns(&res, ASSERTION)
                    {
                        current_audience_group = None;
                    } else if local_name == "SubjectConfirmation" && is_ns(&res, ASSERTION) {
                        current_sc_is_bearer = None;
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

        // Why: in signed mode, only the `<Assertion>` subtree is passed to
        // `verify_assertion_signature`, so falling back to an outer `<Response>/<Issuer>` would
        // let an attacker wrap a signed assertion lacking its own `<Issuer>` inside a forged
        // `<Response>` and spoof `SamlAssertion.issuer`. Signed mode requires the `<Issuer>` to
        // live directly inside the signed `<Assertion>` (SAML 2.0 Core §2.3.3 makes `<Assertion>/<Issuer>`
        // mandatory); `response_issuer` fallback is restricted to unverified diagnostic parsing.
        let issuer = if signed_mode {
            assertion_issuer.ok_or_else(|| {
                SamlError::Malformed("missing Issuer element in signed Assertion".into())
            })?
        } else {
            assertion_issuer
                .or(response_issuer)
                .ok_or_else(|| SamlError::Malformed("missing Issuer element".into()))?
        };
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
            audiences: fields.audiences,
            audience_restrictions: fields.audience_restrictions,
            destination,
            has_response_envelope,
            response_in_response_to,
            subject_confirmations,
            not_before,
            not_on_or_after,
            attributes: fields.attributes,
        })
    }

    /// Validates time conditions and audience restrictions against the assertion.
    ///
    /// This performs a partial assertion check (excluding Web Browser SSO bearer
    /// `<SubjectConfirmation>` enforcement). Use [`Self::validate_with_config`] for
    /// full Web Browser SSO profile validation.
    pub fn validate(
        assertion: &SamlAssertion,
        current_epoch_sec: u64,
        expected_audience: Option<&str>,
    ) -> Result<(), SamlError> {
        Self::validate_with_config(
            assertion,
            current_epoch_sec,
            &SamlValidationConfig {
                expected_audience,
                require_bearer_confirmation: false,
                ..Default::default()
            },
        )
    }

    /// Strictly validates time conditions, audience, issuer, and destination against the assertion.
    ///
    /// This performs a partial assertion check (excluding Web Browser SSO bearer
    /// `<SubjectConfirmation>` enforcement). Use [`Self::validate_with_config`] for
    /// full Web Browser SSO profile validation.
    pub fn validate_strict(
        assertion: &SamlAssertion,
        current_epoch_sec: u64,
        expected_audience: Option<&str>,
        expected_issuer: Option<&str>,
        expected_destination: Option<&str>,
    ) -> Result<(), SamlError> {
        Self::validate_with_config(
            assertion,
            current_epoch_sec,
            &SamlValidationConfig {
                expected_audience,
                expected_issuer,
                expected_destination,
                require_bearer_confirmation: false,
                ..Default::default()
            },
        )
    }

    /// Validates a SAML assertion against full SAML 2.0 Web Browser SSO constraints,
    /// including clock skew, multi-audience OR-semantics, `SubjectConfirmationData`
    /// (`Recipient`, `NotOnOrAfter`, `InResponseTo`), and replay protection.
    pub fn validate_with_config(
        assertion: &SamlAssertion,
        current_epoch_sec: u64,
        config: &SamlValidationConfig<'_>,
    ) -> Result<(), SamlError> {
        let skew = config.clock_skew_secs;

        if let Some(not_before) = assertion.not_before
            && current_epoch_sec.saturating_add(skew) < not_before
        {
            return Err(SamlError::NotYetValid {
                not_before,
                current: current_epoch_sec,
            });
        }

        let not_on_or_after = assertion
            .not_on_or_after
            .ok_or(SamlError::MissingExpirationCondition)?;

        if current_epoch_sec >= not_on_or_after.saturating_add(skew) {
            return Err(SamlError::Expired {
                not_on_or_after,
                current: current_epoch_sec,
            });
        }

        if let Some(expected) = config.expected_audience {
            // Why: SAML 2.0 Core §2.5.1.4 requires OR within each `<AudienceRestriction>`
            // and AND across multiple `<AudienceRestriction>` elements.
            let matched = !assertion.audience_restrictions.is_empty()
                && assertion
                    .audience_restrictions
                    .iter()
                    .all(|group| group.iter().any(|a| a == expected));
            if !matched {
                let received = if assertion.audiences.is_empty() {
                    "<none>".to_string()
                } else {
                    assertion.audiences.join(", ")
                };
                return Err(SamlError::AudienceMismatch {
                    expected: expected.to_string(),
                    received,
                });
            }
        }

        if let Some(expected) = config.expected_issuer
            && assertion.issuer != expected
        {
            return Err(SamlError::IssuerMismatch {
                expected: expected.to_string(),
                received: assertion.issuer.clone(),
            });
        }

        if let Some(expected) = config.expected_destination {
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

        if let Some(expected) = config.expected_in_response_to {
            // Why: SAML Core requires a solicited `<Response>` envelope to carry the
            // matching `InResponseTo` attribute in addition to the signed bearer
            // `<SubjectConfirmationData/@InResponseTo>` check below.
            if assertion.has_response_envelope {
                match assertion.response_in_response_to.as_deref() {
                    Some(r) if r == expected => {}
                    Some(r) => {
                        return Err(SamlError::InResponseToMismatch {
                            expected: expected.to_string(),
                            received: r.to_string(),
                        });
                    }
                    None => {
                        return Err(SamlError::InResponseToMismatch {
                            expected: expected.to_string(),
                            received: "<none>".to_string(),
                        });
                    }
                }
            } else if let Some(r) = assertion.response_in_response_to.as_deref()
                && r != expected
            {
                return Err(SamlError::InResponseToMismatch {
                    expected: expected.to_string(),
                    received: r.to_string(),
                });
            }
        }

        // Why: Web Browser SSO permits multiple bearer `<SubjectConfirmation>` alternatives
        // and succeeds if at least one confirmation satisfies `NotOnOrAfter` (required on
        // each eligible bearer confirmation), `Recipient`, and `InResponseTo` together.
        // When `expected_in_response_to` is configured, never fall back to the unsigned outer
        // `Response/@InResponseTo` — require correlation from the signed bearer confirmation.
        if !assertion.subject_confirmations.is_empty() {
            let mut last_err: Option<SamlError> = None;
            let mut any_valid = false;
            for scd in &assertion.subject_confirmations {
                let Some(scd_exp) = scd.not_on_or_after else {
                    last_err = Some(SamlError::MissingExpirationCondition);
                    continue;
                };
                if current_epoch_sec >= scd_exp.saturating_add(skew) {
                    last_err = Some(SamlError::Expired {
                        not_on_or_after: scd_exp,
                        current: current_epoch_sec,
                    });
                    continue;
                }
                if let Some(expected) = config.expected_recipient {
                    match scd.recipient.as_deref() {
                        Some(r) if r == expected => {}
                        Some(r) => {
                            last_err = Some(SamlError::RecipientMismatch {
                                expected: expected.to_string(),
                                received: r.to_string(),
                            });
                            continue;
                        }
                        None => {
                            last_err = Some(SamlError::RecipientMismatch {
                                expected: expected.to_string(),
                                received: "<none>".to_string(),
                            });
                            continue;
                        }
                    }
                }
                if let Some(expected) = config.expected_in_response_to {
                    match scd.in_response_to.as_deref() {
                        Some(r) if r == expected => {}
                        Some(r) => {
                            last_err = Some(SamlError::InResponseToMismatch {
                                expected: expected.to_string(),
                                received: r.to_string(),
                            });
                            continue;
                        }
                        None => {
                            last_err = Some(SamlError::InResponseToMismatch {
                                expected: expected.to_string(),
                                received: "<none>".to_string(),
                            });
                            continue;
                        }
                    }
                }
                any_valid = true;
                break;
            }
            if !any_valid {
                return Err(last_err.unwrap_or_else(|| {
                    SamlError::Malformed("no valid bearer SubjectConfirmationData".into())
                }));
            }
        } else {
            if let Some(expected) = config.expected_recipient {
                return Err(SamlError::RecipientMismatch {
                    expected: expected.to_string(),
                    received: "<none>".to_string(),
                });
            }
            if let Some(expected) = config.expected_in_response_to {
                return Err(SamlError::InResponseToMismatch {
                    expected: expected.to_string(),
                    received: "<none>".to_string(),
                });
            }
            if config.require_bearer_confirmation {
                return Err(SamlError::Malformed(
                    "missing required bearer SubjectConfirmation".into(),
                ));
            }
        }

        if let Some(cache) = config.replay_cache {
            // Why: retain the replay record through the full skew-adjusted validity
            // window (`not_on_or_after + clock_skew_secs`) so pruning at `not_on_or_after`
            // cannot allow a replay while validation would still accept the assertion.
            let replay_expires_at = not_on_or_after.saturating_add(skew);
            let fresh = cache.record_if_absent(&assertion.id, replay_expires_at)?;
            if !fresh {
                return Err(SamlError::AssertionReplayed(assertion.id.clone()));
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
            assertion.audiences,
            vec!["https://sp.example.com/a&b?v=1".to_string()]
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

    #[test]
    fn signature_subtree_injected_subject_conditions_and_attributes_are_ignored() {
        struct AcceptVerifier;
        impl SamlSignatureVerifier for AcceptVerifier {
            fn verify_assertion_signature(
                &self,
                _: &str,
                _: &str,
                _: &str,
            ) -> Result<(), SamlError> {
                Ok(())
            }
        }

        let xml = r##"<saml:Assertion xmlns:saml="urn:oasis:names:tc:SAML:2.0:assertion" xmlns:ds="http://www.w3.org/2000/09/xmldsig#" ID="_a_xsw_sig">
            <saml:Issuer>https://idp.example.com</saml:Issuer>
            <ds:Signature>
                <ds:SignedInfo>
                    <ds:Reference URI="#_a_xsw_sig"/>
                </ds:SignedInfo>
                <ds:Object>
                    <saml:Subject>
                        <saml:NameID>alice@acme.example</saml:NameID>
                    </saml:Subject>
                    <saml:Conditions NotBefore="2020-01-01T00:00:00Z" NotOnOrAfter="2099-01-01T00:00:00Z">
                        <saml:AudienceRestriction>
                            <saml:Audience>https://forged-sp.example.com</saml:Audience>
                        </saml:AudienceRestriction>
                    </saml:Conditions>
                    <saml:AttributeStatement>
                        <saml:Attribute Name="roles">
                            <saml:AttributeValue>admin</saml:AttributeValue>
                        </saml:Attribute>
                    </saml:AttributeStatement>
                </ds:Object>
            </ds:Signature>
            <saml:Subject>
                <saml:NameID>mallory@acme.example</saml:NameID>
            </saml:Subject>
            <saml:Conditions NotBefore="2026-11-02T11:00:00Z" NotOnOrAfter="2026-11-02T13:00:00Z">
                <saml:AudienceRestriction>
                    <saml:Audience>https://real-sp.example.com</saml:Audience>
                </saml:AudienceRestriction>
            </saml:Conditions>
            <saml:AttributeStatement>
                <saml:Attribute Name="roles">
                    <saml:AttributeValue>member</saml:AttributeValue>
                </saml:Attribute>
            </saml:AttributeStatement>
        </saml:Assertion>"##;

        let assertion = SamlParser::parse_signed(xml, &AcceptVerifier)
            .expect("valid signed assertion with ds:Object payload must parse real claims");
        assert_eq!(
            assertion.subject, "mallory@acme.example",
            "Subject injected inside ds:Signature/ds:Object must be ignored"
        );
        assert_eq!(
            assertion.audiences,
            vec!["https://real-sp.example.com".to_string()],
            "Audience injected inside ds:Signature/ds:Object must be ignored"
        );
        assert_eq!(
            assertion.attributes.get("roles"),
            Some(&vec!["member".to_string()]),
            "Attribute values injected inside ds:Signature/ds:Object must be ignored"
        );
    }

    #[test]
    fn signed_mode_rejects_unsigned_response_issuer_fallback_when_assertion_lacks_issuer() {
        struct AcceptVerifier;
        impl SamlSignatureVerifier for AcceptVerifier {
            fn verify_assertion_signature(
                &self,
                _: &str,
                _: &str,
                _: &str,
            ) -> Result<(), SamlError> {
                Ok(())
            }
        }

        let xml = r##"<saml2p:Response xmlns:saml2p="urn:oasis:names:tc:SAML:2.0:protocol" xmlns:saml="urn:oasis:names:tc:SAML:2.0:assertion" xmlns:ds="http://www.w3.org/2000/09/xmldsig#" ID="_resp_forged">
            <saml:Issuer>https://attacker-chosen-issuer.example.com</saml:Issuer>
            <saml:Assertion ID="_assert_no_issuer">
                <ds:Signature>
                    <ds:SignedInfo>
                        <ds:Reference URI="#_assert_no_issuer"/>
                    </ds:SignedInfo>
                </ds:Signature>
                <saml:Subject>
                    <saml:NameID>user@enterprise.com</saml:NameID>
                </saml:Subject>
            </saml:Assertion>
        </saml2p:Response>"##;

        let err = SamlParser::parse_signed(xml, &AcceptVerifier)
            .expect_err("signed assertion missing its own <Issuer> must not fall back to unsigned Response <Issuer>");
        assert!(matches!(err, SamlError::Malformed(_)));
    }

    #[test]
    fn advice_extension_audience_cannot_override_assertion_conditions_audience() {
        let xml = r#"<saml:Assertion xmlns:saml="urn:oasis:names:tc:SAML:2.0:assertion" xmlns:ext="urn:example:ext" ID="_a_adv_aud">
            <saml:Issuer>https://idp.example.com</saml:Issuer>
            <saml:Subject>
                <saml:NameID>alice@acme.example</saml:NameID>
            </saml:Subject>
            <saml:Advice>
                <ext:Payload>
                    <saml:AudienceRestriction>
                        <saml:Audience>https://forged-advice-sp.example.com</saml:Audience>
                    </saml:AudienceRestriction>
                </ext:Payload>
            </saml:Advice>
            <saml:Conditions NotBefore="2026-11-02T11:00:00Z" NotOnOrAfter="2026-11-02T13:00:00Z">
                <saml:AudienceRestriction>
                    <saml:Audience>https://real-sp.example.com</saml:Audience>
                </saml:AudienceRestriction>
            </saml:Conditions>
        </saml:Assertion>"#;

        let assertion = SamlParser::parse_unverified(xml)
            .expect("valid assertion with Advice extension must parse");
        assert_eq!(
            assertion.audiences,
            vec!["https://real-sp.example.com".to_string()],
            "Audience nested under Advice/ext:Payload must not override Assertion/Conditions/AudienceRestriction/Audience"
        );
    }

    #[test]
    fn duplicate_id_attribute_triggers_signature_wrapping_detected() {
        let xml = r#"<samlp:Response xmlns:samlp="urn:oasis:names:tc:SAML:2.0:protocol" xmlns:saml="urn:oasis:names:tc:SAML:2.0:assertion" ID="_dup">
            <saml:Assertion ID="_dup">
                <saml:Issuer>https://idp.example.com</saml:Issuer>
                <saml:Subject><saml:NameID>alice@example.org</saml:NameID></saml:Subject>
            </saml:Assertion>
        </samlp:Response>"#;
        let err = SamlParser::parse_unverified(xml).unwrap_err();
        assert!(matches!(err, SamlError::SignatureWrappingDetected(_)));
    }

    #[test]
    fn multi_audience_matches_second_entry_and_subject_confirmation_and_replay_cache_work() {
        let xml = r#"<samlp:Response xmlns:samlp="urn:oasis:names:tc:SAML:2.0:protocol" xmlns:saml="urn:oasis:names:tc:SAML:2.0:assertion" ID="_r1" Destination="https://sp.example.com/acs" InResponseTo="_req_42">
            <saml:Assertion ID="_a_multi">
                <saml:Issuer>https://idp.example.com</saml:Issuer>
                <saml:Subject>
                    <saml:NameID>alice@example.org</saml:NameID>
                    <saml:SubjectConfirmation Method="urn:oasis:names:tc:SAML:2.0:cm:bearer">
                        <saml:SubjectConfirmationData Recipient="https://sp.example.com/acs" NotOnOrAfter="2030-01-01T00:00:00Z" InResponseTo="_req_42"/>
                    </saml:SubjectConfirmation>
                </saml:Subject>
                <saml:Conditions NotBefore="2020-01-01T00:00:00Z" NotOnOrAfter="2030-01-01T00:00:00Z">
                    <saml:AudienceRestriction>
                        <saml:Audience>https://other-sp.example.com</saml:Audience>
                        <saml:Audience>https://sp.example.com</saml:Audience>
                    </saml:AudienceRestriction>
                </saml:Conditions>
            </saml:Assertion>
        </samlp:Response>"#;

        let assertion = SamlParser::parse_unverified(xml).expect("valid assertion");
        assert_eq!(assertion.audiences.len(), 2);

        let now = parse_iso8601_strict("2025-06-01T00:00:00Z").unwrap();
        let replay_cache = InMemorySamlReplayCache::new();
        let cfg = SamlValidationConfig {
            expected_audience: Some("https://sp.example.com"),
            expected_issuer: Some("https://idp.example.com"),
            expected_destination: Some("https://sp.example.com/acs"),
            expected_recipient: Some("https://sp.example.com/acs"),
            expected_in_response_to: Some("_req_42"),
            clock_skew_secs: 30,
            replay_cache: Some(&replay_cache),
            ..Default::default()
        };
        SamlParser::validate_with_config(&assertion, now, &cfg).expect("first use succeeds");

        // Second use of the same assertion ID must fail with AssertionReplayed.
        let replay_err = SamlParser::validate_with_config(&assertion, now, &cfg).unwrap_err();
        assert!(matches!(replay_err, SamlError::AssertionReplayed(_)));

        // Wrong Recipient and wrong InResponseTo fail closed.
        let bad_recipient = SamlParser::validate_with_config(
            &assertion,
            now,
            &SamlValidationConfig {
                expected_recipient: Some("https://wrong.example.com/acs"),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(matches!(bad_recipient, SamlError::RecipientMismatch { .. }));

        let bad_irt = SamlParser::validate_with_config(
            &assertion,
            now,
            &SamlValidationConfig {
                expected_in_response_to: Some("_req_999"),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(matches!(bad_irt, SamlError::InResponseToMismatch { .. }));
    }

    #[test]
    fn normalized_attribute_unescapes_recipient_entities() {
        let xml = r#"<samlp:Response xmlns:samlp="urn:oasis:names:tc:SAML:2.0:protocol" xmlns:saml="urn:oasis:names:tc:SAML:2.0:assertion" ID="_r_esc">
            <saml:Assertion ID="_a_esc">
                <saml:Issuer>https://idp.example.com</saml:Issuer>
                <saml:Subject>
                    <saml:NameID>alice@example.org</saml:NameID>
                    <saml:SubjectConfirmation Method="urn:oasis:names:tc:SAML:2.0:cm:bearer">
                        <saml:SubjectConfirmationData Recipient="https://sp.example/acs?a=1&amp;b=2" NotOnOrAfter="2030-01-01T00:00:00Z"/>
                    </saml:SubjectConfirmation>
                </saml:Subject>
                <saml:Conditions NotBefore="2020-01-01T00:00:00Z" NotOnOrAfter="2030-01-01T00:00:00Z"/>
            </saml:Assertion>
        </samlp:Response>"#;
        let assertion = SamlParser::parse_unverified(xml).expect("valid escaped recipient");
        let now = parse_iso8601_strict("2025-06-01T00:00:00Z").unwrap();
        SamlParser::validate_with_config(
            &assertion,
            now,
            &SamlValidationConfig {
                expected_recipient: Some("https://sp.example/acs?a=1&b=2"),
                ..Default::default()
            },
        )
        .expect("normalized &amp; in Recipient must match unescaped URL");
    }

    #[test]
    fn multiple_bearer_subject_confirmations_succeed_when_first_is_valid_and_second_is_invalid() {
        let xml = r#"<samlp:Response xmlns:samlp="urn:oasis:names:tc:SAML:2.0:protocol" xmlns:saml="urn:oasis:names:tc:SAML:2.0:assertion" ID="_r_sc" InResponseTo="_req_ok">
            <saml:Assertion ID="_a_sc">
                <saml:Issuer>https://idp.example.com</saml:Issuer>
                <saml:Subject>
                    <saml:NameID>alice@example.org</saml:NameID>
                    <saml:SubjectConfirmation Method="urn:oasis:names:tc:SAML:2.0:cm:bearer">
                        <saml:SubjectConfirmationData Recipient="https://sp.example/acs" NotOnOrAfter="2030-01-01T00:00:00Z" InResponseTo="_req_ok"/>
                    </saml:SubjectConfirmation>
                    <saml:SubjectConfirmation Method="urn:oasis:names:tc:SAML:2.0:cm:bearer">
                        <saml:SubjectConfirmationData Recipient="https://other-sp.example/acs" NotOnOrAfter="2020-01-01T00:00:00Z" InResponseTo="_req_other"/>
                    </saml:SubjectConfirmation>
                </saml:Subject>
                <saml:Conditions NotBefore="2020-01-01T00:00:00Z" NotOnOrAfter="2030-01-01T00:00:00Z"/>
            </saml:Assertion>
        </samlp:Response>"#;
        let assertion = SamlParser::parse_unverified(xml).expect("valid assertion");
        assert_eq!(assertion.subject_confirmations.len(), 2);
        let now = parse_iso8601_strict("2025-06-01T00:00:00Z").unwrap();
        SamlParser::validate_with_config(
            &assertion,
            now,
            &SamlValidationConfig {
                expected_recipient: Some("https://sp.example/acs"),
                expected_in_response_to: Some("_req_ok"),
                ..Default::default()
            },
        )
        .expect("valid first bearer SubjectConfirmation must not be overwritten by second");
    }

    #[test]
    fn multiple_audience_restriction_groups_require_and_across_groups() {
        let xml = r#"<samlp:Response xmlns:samlp="urn:oasis:names:tc:SAML:2.0:protocol" xmlns:saml="urn:oasis:names:tc:SAML:2.0:assertion" ID="_r_aud">
            <saml:Assertion ID="_a_aud">
                <saml:Issuer>https://idp.example.com</saml:Issuer>
                <saml:Subject><saml:NameID>alice@example.org</saml:NameID></saml:Subject>
                <saml:Conditions NotBefore="2020-01-01T00:00:00Z" NotOnOrAfter="2030-01-01T00:00:00Z">
                    <saml:AudienceRestriction>
                        <saml:Audience>https://other-sp.example.com</saml:Audience>
                    </saml:AudienceRestriction>
                    <saml:AudienceRestriction>
                        <saml:Audience>https://this-sp.example.com</saml:Audience>
                    </saml:AudienceRestriction>
                </saml:Conditions>
            </saml:Assertion>
        </samlp:Response>"#;
        let assertion = SamlParser::parse_unverified(xml).expect("valid assertion");
        let now = parse_iso8601_strict("2025-06-01T00:00:00Z").unwrap();
        let err = SamlParser::validate_with_config(
            &assertion,
            now,
            &SamlValidationConfig {
                expected_audience: Some("https://this-sp.example.com"),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(
            matches!(err, SamlError::AudienceMismatch { .. }),
            "separate <AudienceRestriction> groups require AND semantics"
        );
    }

    #[test]
    fn expected_in_response_to_does_not_fall_back_to_unsigned_response_attribute() {
        let xml = r#"<samlp:Response xmlns:samlp="urn:oasis:names:tc:SAML:2.0:protocol" xmlns:saml="urn:oasis:names:tc:SAML:2.0:assertion" ID="_r_irt" InResponseTo="_req_forged">
            <saml:Assertion ID="_a_irt">
                <saml:Issuer>https://idp.example.com</saml:Issuer>
                <saml:Subject>
                    <saml:NameID>alice@example.org</saml:NameID>
                    <saml:SubjectConfirmation Method="urn:oasis:names:tc:SAML:2.0:cm:bearer">
                        <saml:SubjectConfirmationData Recipient="https://sp.example/acs" NotOnOrAfter="2030-01-01T00:00:00Z"/>
                    </saml:SubjectConfirmation>
                </saml:Subject>
                <saml:Conditions NotBefore="2020-01-01T00:00:00Z" NotOnOrAfter="2030-01-01T00:00:00Z"/>
            </saml:Assertion>
        </samlp:Response>"#;
        let assertion = SamlParser::parse_unverified(xml).expect("valid assertion");
        let now = parse_iso8601_strict("2025-06-01T00:00:00Z").unwrap();
        let err = SamlParser::validate_with_config(
            &assertion,
            now,
            &SamlValidationConfig {
                expected_in_response_to: Some("_req_forged"),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(
            matches!(err, SamlError::InResponseToMismatch { .. }),
            "must not accept unsigned Response/@InResponseTo when SubjectConfirmationData omits InResponseTo"
        );
    }

    #[test]
    fn replay_cache_retains_entry_through_clock_skew_window() {
        let xml = r#"<samlp:Response xmlns:samlp="urn:oasis:names:tc:SAML:2.0:protocol" xmlns:saml="urn:oasis:names:tc:SAML:2.0:assertion" ID="_r_skew">
            <saml:Assertion ID="_a_skew">
                <saml:Issuer>https://idp.example.com</saml:Issuer>
                <saml:Subject>
                    <saml:NameID>alice@example.org</saml:NameID>
                    <saml:SubjectConfirmation Method="urn:oasis:names:tc:SAML:2.0:cm:bearer">
                        <saml:SubjectConfirmationData NotOnOrAfter="1970-01-01T00:16:40Z"/>
                    </saml:SubjectConfirmation>
                </saml:Subject>
                <saml:Conditions NotBefore="1970-01-01T00:00:01Z" NotOnOrAfter="1970-01-01T00:16:40Z"/>
            </saml:Assertion>
        </samlp:Response>"#;
        let assertion = SamlParser::parse_unverified(xml).expect("valid assertion");
        assert_eq!(assertion.not_on_or_after, Some(1000));

        let cache = InMemorySamlReplayCache::new();
        let cfg = SamlValidationConfig {
            clock_skew_secs: 30,
            replay_cache: Some(&cache),
            ..Default::default()
        };
        SamlParser::validate_with_config(&assertion, 999, &cfg).expect("initial validation ok");

        // Prune at exact NotOnOrAfter (1000): the entry must survive through 1000 + 30 = 1030.
        cache.prune_expired(1000);
        let err = SamlParser::validate_with_config(&assertion, 1001, &cfg).unwrap_err();
        assert!(
            matches!(err, SamlError::AssertionReplayed(_)),
            "replay within skew window must still be rejected after pruning at not_on_or_after"
        );
    }

    #[test]
    fn typed_condition_audience_restriction_is_enforced_and_unknown_condition_fails_closed() {
        let xml_excluded = r#"<samlp:Response xmlns:samlp="urn:oasis:names:tc:SAML:2.0:protocol" xmlns:saml="urn:oasis:names:tc:SAML:2.0:assertion" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" ID="_r_cond">
            <saml:Assertion ID="_a_cond">
                <saml:Issuer>https://idp.example.com</saml:Issuer>
                <saml:Subject>
                    <saml:NameID>alice@example.org</saml:NameID>
                    <saml:SubjectConfirmation Method="urn:oasis:names:tc:SAML:2.0:cm:bearer">
                        <saml:SubjectConfirmationData NotOnOrAfter="2030-01-01T00:00:00Z"/>
                    </saml:SubjectConfirmation>
                </saml:Subject>
                <saml:Conditions NotBefore="2020-01-01T00:00:00Z" NotOnOrAfter="2030-01-01T00:00:00Z">
                    <saml:AudienceRestriction>
                        <saml:Audience>https://this-sp.example.com</saml:Audience>
                    </saml:AudienceRestriction>
                    <saml:Condition xsi:type="saml:AudienceRestrictionType">
                        <saml:Audience>https://other-sp.example.com</saml:Audience>
                    </saml:Condition>
                </saml:Conditions>
            </saml:Assertion>
        </samlp:Response>"#;
        let assertion = SamlParser::parse_unverified(xml_excluded).expect("typed condition parses");
        let now = parse_iso8601_strict("2025-06-01T00:00:00Z").unwrap();
        let err = SamlParser::validate_with_config(
            &assertion,
            now,
            &SamlValidationConfig {
                expected_audience: Some("https://this-sp.example.com"),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(matches!(err, SamlError::AudienceMismatch { .. }));

        let xml_unknown = r#"<samlp:Response xmlns:samlp="urn:oasis:names:tc:SAML:2.0:protocol" xmlns:saml="urn:oasis:names:tc:SAML:2.0:assertion" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" ID="_r_unk">
            <saml:Assertion ID="_a_unk">
                <saml:Issuer>https://idp.example.com</saml:Issuer>
                <saml:Subject><saml:NameID>alice@example.org</saml:NameID></saml:Subject>
                <saml:Conditions NotBefore="2020-01-01T00:00:00Z" NotOnOrAfter="2030-01-01T00:00:00Z">
                    <saml:Condition xsi:type="custom:CustomConditionType"/>
                </saml:Conditions>
            </saml:Assertion>
        </samlp:Response>"#;
        let err_unknown = SamlParser::parse_unverified(xml_unknown).unwrap_err();
        assert!(matches!(err_unknown, SamlError::Malformed(_)));

        // A foreign namespace prefix bound to `AudienceRestrictionType` must NOT be
        // treated as SAML's built-in AudienceRestrictionType; it must fail closed.
        let xml_foreign_ns = r#"<samlp:Response xmlns:samlp="urn:oasis:names:tc:SAML:2.0:protocol" xmlns:saml="urn:oasis:names:tc:SAML:2.0:assertion" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" ID="_r_fns">
            <saml:Assertion ID="_a_fns">
                <saml:Issuer>https://idp.example.com</saml:Issuer>
                <saml:Subject><saml:NameID>alice@example.org</saml:NameID></saml:Subject>
                <saml:Conditions NotBefore="2020-01-01T00:00:00Z" NotOnOrAfter="2030-01-01T00:00:00Z">
                    <saml:Condition xmlns:ext="urn:example:ext" xsi:type="ext:AudienceRestrictionType">
                        <saml:Audience>https://this-sp.example.com</saml:Audience>
                    </saml:Condition>
                </saml:Conditions>
            </saml:Assertion>
        </samlp:Response>"#;
        let err_foreign = SamlParser::parse_unverified(xml_foreign_ns).unwrap_err();
        assert!(matches!(err_foreign, SamlError::Malformed(_)));
    }

    #[test]
    fn subject_confirmation_requires_explicit_bearer_method_and_not_on_or_after_and_outer_in_response_to()
     {
        let now = parse_iso8601_strict("2025-06-01T00:00:00Z").unwrap();

        // Missing Method attribute on SubjectConfirmation must NOT be treated as bearer.
        let xml_no_method = r#"<samlp:Response xmlns:samlp="urn:oasis:names:tc:SAML:2.0:protocol" xmlns:saml="urn:oasis:names:tc:SAML:2.0:assertion" ID="_r_nm">
            <saml:Assertion ID="_a_nm">
                <saml:Issuer>https://idp.example.com</saml:Issuer>
                <saml:Subject>
                    <saml:NameID>alice@example.org</saml:NameID>
                    <saml:SubjectConfirmation>
                        <saml:SubjectConfirmationData Recipient="https://sp.example/acs" NotOnOrAfter="2030-01-01T00:00:00Z"/>
                    </saml:SubjectConfirmation>
                </saml:Subject>
                <saml:Conditions NotBefore="2020-01-01T00:00:00Z" NotOnOrAfter="2030-01-01T00:00:00Z"/>
            </saml:Assertion>
        </samlp:Response>"#;
        let a_no_method = SamlParser::parse_unverified(xml_no_method).expect("parses");
        assert!(a_no_method.subject_confirmations.is_empty());
        assert!(matches!(
            SamlParser::validate_with_config(&a_no_method, now, &SamlValidationConfig::default()),
            Err(SamlError::Malformed(_))
        ));

        // Holder-of-key only confirmation must be rejected by default Web Browser SSO validation.
        let xml_hok = r#"<samlp:Response xmlns:samlp="urn:oasis:names:tc:SAML:2.0:protocol" xmlns:saml="urn:oasis:names:tc:SAML:2.0:assertion" ID="_r_hok">
            <saml:Assertion ID="_a_hok">
                <saml:Issuer>https://idp.example.com</saml:Issuer>
                <saml:Subject>
                    <saml:NameID>alice@example.org</saml:NameID>
                    <saml:SubjectConfirmation Method="urn:oasis:names:tc:SAML:2.0:cm:holder-of-key">
                        <saml:SubjectConfirmationData NotOnOrAfter="2030-01-01T00:00:00Z"/>
                    </saml:SubjectConfirmation>
                </saml:Subject>
                <saml:Conditions NotBefore="2020-01-01T00:00:00Z" NotOnOrAfter="2030-01-01T00:00:00Z"/>
            </saml:Assertion>
        </samlp:Response>"#;
        let a_hok = SamlParser::parse_unverified(xml_hok).expect("parses");
        assert!(matches!(
            SamlParser::validate_with_config(&a_hok, now, &SamlValidationConfig::default()),
            Err(SamlError::Malformed(_))
        ));

        // Bearer confirmation lacking NotOnOrAfter must be rejected; a 2nd valid one can still pass.
        let xml_no_exp = r#"<samlp:Response xmlns:samlp="urn:oasis:names:tc:SAML:2.0:protocol" xmlns:saml="urn:oasis:names:tc:SAML:2.0:assertion" ID="_r_ne">
            <saml:Assertion ID="_a_ne">
                <saml:Issuer>https://idp.example.com</saml:Issuer>
                <saml:Subject>
                    <saml:NameID>alice@example.org</saml:NameID>
                    <saml:SubjectConfirmation Method="urn:oasis:names:tc:SAML:2.0:cm:bearer">
                        <saml:SubjectConfirmationData Recipient="https://sp.example/acs"/>
                    </saml:SubjectConfirmation>
                </saml:Subject>
                <saml:Conditions NotBefore="2020-01-01T00:00:00Z" NotOnOrAfter="2030-01-01T00:00:00Z"/>
            </saml:Assertion>
        </samlp:Response>"#;
        let a_no_exp = SamlParser::parse_unverified(xml_no_exp).expect("parses");
        assert!(matches!(
            SamlParser::validate_with_config(&a_no_exp, now, &SamlValidationConfig::default()),
            Err(SamlError::MissingExpirationCondition)
        ));

        // Solicited Response omitting outer Response/@InResponseTo must fail closed even if SCD has it.
        let xml_missing_outer_irt = r#"<samlp:Response xmlns:samlp="urn:oasis:names:tc:SAML:2.0:protocol" xmlns:saml="urn:oasis:names:tc:SAML:2.0:assertion" ID="_r_mo">
            <saml:Assertion ID="_a_mo">
                <saml:Issuer>https://idp.example.com</saml:Issuer>
                <saml:Subject>
                    <saml:NameID>alice@example.org</saml:NameID>
                    <saml:SubjectConfirmation Method="urn:oasis:names:tc:SAML:2.0:cm:bearer">
                        <saml:SubjectConfirmationData Recipient="https://sp.example/acs" NotOnOrAfter="2030-01-01T00:00:00Z" InResponseTo="_req_ok"/>
                    </saml:SubjectConfirmation>
                </saml:Subject>
                <saml:Conditions NotBefore="2020-01-01T00:00:00Z" NotOnOrAfter="2030-01-01T00:00:00Z"/>
            </saml:Assertion>
        </samlp:Response>"#;
        let a_missing_outer = SamlParser::parse_unverified(xml_missing_outer_irt).expect("parses");
        assert!(matches!(
            SamlParser::validate_with_config(
                &a_missing_outer,
                now,
                &SamlValidationConfig {
                    expected_in_response_to: Some("_req_ok"),
                    ..Default::default()
                }
            ),
            Err(SamlError::InResponseToMismatch { .. })
        ));
    }
}
