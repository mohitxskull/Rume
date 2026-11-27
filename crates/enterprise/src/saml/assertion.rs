use super::verify::{SamlError, SamlSignatureVerifier};
use quick_xml::events::Event;
use quick_xml::name::ResolveResult;
use quick_xml::reader::NsReader;
use saml_rs::constants::namespace::{ASSERTION, DSIG, PROTOCOL};
use std::collections::{BTreeMap, BTreeSet};

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
/// Parsed `<saml:SubjectConfirmationData>` attributes (SAML 2.0 Core §2.4.1.2).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SamlSubjectConfirmationData {
    /// ACS recipient URL (`Recipient` attribute).
    pub recipient: Option<String>,
    /// Confirmation expiration timestamp (`NotOnOrAfter` attribute, epoch seconds).
    pub not_on_or_after: Option<u64>,
    /// Subsecond nanoseconds (`0..1_000_000_000`) for `not_on_or_after`.
    pub not_on_or_after_subsec_nanos: u32,
    /// Correlated AuthnRequest ID (`InResponseTo` attribute).
    pub in_response_to: Option<String>,
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
    /// Whether the assertion contains a direct `<saml:AuthnStatement>` child
    /// (required by SAML 2.0 Web Browser SSO profile).
    pub has_authn_statement: bool,
    /// Parsed bearer `<saml:SubjectConfirmationData>` alternatives (SAML 2.0 Web Browser SSO
    /// permits multiple bearer confirmations and succeeds if at least one satisfies all checks).
    pub subject_confirmations: Vec<SamlSubjectConfirmationData>,
    /// NotBefore epoch seconds condition.
    pub not_before: Option<u64>,
    /// Subsecond nanoseconds (`0..1_000_000_000`) for `not_before`.
    pub not_before_subsec_nanos: u32,
    /// NotOnOrAfter epoch seconds condition.
    pub not_on_or_after: Option<u64>,
    /// Subsecond nanoseconds (`0..1_000_000_000`) for `not_on_or_after`.
    pub not_on_or_after_subsec_nanos: u32,
    /// Extracted user claims and attributes.
    pub attributes: BTreeMap<String, Vec<String>>,
    /// Whether this assertion was produced after cryptographic signature verification.
    ///
    /// Kept private so callers cannot forge verified status on an assertion parsed via
    /// [`SamlParser::parse_unverified`].
    signature_verified: bool,
}

impl SamlAssertion {
    /// Returns `true` if this assertion was produced by a signature-verified parse
    /// ([`SamlParser::parse_signed`] or [`SamlParser::parse_with_verifier`]).
    pub fn is_signature_verified(&self) -> bool {
        self.signature_verified
    }
}
/// Parses an ISO 8601 / RFC 3339 timestamp strictly, returning `(epoch_secs, subsec_nanos)`.
pub(crate) fn parse_iso8601_strict_parts(s: &str) -> Result<(u64, u32), SamlError> {
    let trimmed = s.trim();

    // Why: delegate calendar, leap-year, and `-23:59..=+23:59` offset validation to
    // `chrono::DateTime::parse_from_rfc3339` (already pinned in `Cargo.lock`) instead of
    // hand-rolled date math and UTF-8 byte slicing that can panic on multi-byte inputs; check
    // byte index 10 for `'T'`/`'t'` to reject RFC 3339 space separators, use `u64::try_from`
    // so pre-1970 instants fail closed instead of wrapping, and reject leap-second fractions
    // (`subsec_nanos >= 1_000_000_000`) so the public `0..1_000_000_000` invariant always holds.
    if !matches!(trimmed.as_bytes().get(10), Some(b'T' | b't')) {
        return Err(SamlError::Malformed(format!(
            "invalid ISO 8601 timestamp '{trimmed}': missing 'T' separator"
        )));
    }

    let dt = chrono::DateTime::parse_from_rfc3339(trimmed).map_err(|err| {
        SamlError::Malformed(format!("invalid ISO 8601 timestamp '{trimmed}': {err}"))
    })?;

    let secs = u64::try_from(dt.timestamp()).map_err(|_| {
        SamlError::Malformed(format!("timestamp '{trimmed}' resolves before Unix epoch"))
    })?;
    let nanos = dt.timestamp_subsec_nanos();
    if nanos >= 1_000_000_000 {
        return Err(SamlError::Malformed(format!(
            "leap-second timestamp '{trimmed}' is not supported"
        )));
    }
    Ok((secs, nanos))
}

/// Parses an ISO 8601 / RFC 3339 timestamp strictly, including UTC `'Z'`, fractional seconds,
/// and explicit colon-separated timezone offsets (`±HH:MM`).
///
/// An explicit timezone (`'Z'` or `±HH:MM`) and a `'T'`/`'t'` date-time separator are mandatory:
/// raw epoch integers, space-separated RFC 3339 timestamps, naive wall-clock strings without a
/// timezone, and pre-Unix-epoch instants are rejected fail-closed.
pub fn parse_iso8601_strict(s: &str) -> Result<u64, SamlError> {
    parse_iso8601_strict_parts(s).map(|(secs, _)| secs)
}
/// Parser and validator for SAML 2.0 assertions.
pub struct SamlParser;

impl SamlParser {
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
        let mut has_conditions = false;
        let mut not_before: Option<u64> = None;
        let mut not_before_subsec_nanos: u32 = 0;
        let mut not_on_or_after: Option<u64> = None;
        let mut not_on_or_after_subsec_nanos: u32 = 0;
        let mut has_authn_statement = false;
        let mut current_authn_stmt_has_instant = false;

        let mut assertion_start_offset: Option<usize> = None;
        let mut assertion_end_offset: Option<usize> = None;

        let mut signature_references: Vec<String> = Vec::new();

        // Each stack frame carries `(local_name, ns, in_signature_subtree)` for O(1)
        // `<ds:Signature>` ancestor checks on every start event.
        let mut element_stack: Vec<(String, XmlNs, bool)> = Vec::new();
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
                    let parent = element_stack.last().map(|(n, pns, _)| (n.as_str(), *pns));
                    let grandparent = element_stack
                        .len()
                        .checked_sub(2)
                        .and_then(|idx| element_stack.get(idx))
                        .map(|(n, pns, _)| (n.as_str(), *pns));
                    let great_grandparent = element_stack
                        .len()
                        .checked_sub(3)
                        .and_then(|idx| element_stack.get(idx))
                        .map(|(n, pns, _)| (n.as_str(), *pns));
                    // Why: XML-DSig enveloped-signature transforms strip the entire `<ds:Signature>`
                    // subtree before computing the digest. An attacker who holds any validly signed
                    // assertion can inject `<saml:Subject>`, `<saml:Conditions>`, `<saml:Audience>`,
                    // or `<saml:Attribute>` inside `<ds:Signature>` (e.g. `<ds:Object>` or
                    // `<ds:KeyInfo>`) without invalidating the signature. Never extract any SAML
                    // claims while a `<ds:Signature>` element is on the ancestor stack. Carrying the
                    // flag on each stack frame keeps this check O(1) regardless of XML nesting depth.
                    let in_signature = element_stack.last().is_some_and(|(_, _, in_sig)| *in_sig);

                    const XSI_NAMESPACE: &str = "http://www.w3.org/2001/XMLSchema-instance";

                    // Why: parse every attribute strictly (failing closed on malformed
                    // attributes instead of `.flatten()`), reject duplicate `ID`
                    // attributes anywhere in the document to defeat XSW ID-collision
                    // attacks, preserve attribute qualification so only unqualified
                    // attributes populate standard SAML fields (preventing e.g.
                    // `ext:Method` from entering bearer confirmation), and resolve
                    // `xsi:type` by both attribute namespace (`XSI`) and QName prefix.
                    let mut parsed_attrs: Vec<(String, String)> = Vec::new();
                    let mut xsi_type_raw: Option<String> = None;
                    let mut xsi_type_is_saml_audience_restriction = false;
                    for attr_res in e.attributes() {
                        let attr = attr_res.map_err(|err| {
                            SamlError::Malformed(format!("malformed XML attribute: {err}"))
                        })?;
                        let (attr_ns, attr_local) = reader.resolver().resolve_attribute(attr.key);
                        let is_unqualified = matches!(attr_ns, ResolveResult::Unbound);
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
                            // Why: XML Schema Part 2 §3.2.18 mandates whitespace collapse for
                            // `xs:QName` values (e.g. `xsi:type=" saml:AudienceRestrictionType"`),
                            // whereas `quick_xml::Attribute::normalized_value` preserves leading
                            // and trailing spaces. Trim ONLY the four XML whitespace characters:
                            // `str::trim()` also strips Unicode whitespace such as U+1680, which is
                            // a legal XML name character, so it could rewrite a foreign prefix
                            // (`\u{1680}saml`) into the SAML prefix (`saml`) and bypass the
                            // namespace guard.
                            let trimmed = v.trim_matches([' ', '\t', '\r', '\n']);
                            let (type_ns, type_local) = reader
                                .resolver()
                                .resolve_element(quick_xml::name::QName(trimmed));
                            xsi_type_is_saml_audience_restriction = is_ns(&type_ns, ASSERTION)
                                && type_local.as_ref() == "AudienceRestrictionType";
                            xsi_type_raw = Some(trimmed.to_string());
                        }
                        if is_unqualified {
                            if k == "ID" && !v.is_empty() && !seen_ids.insert(v.clone()) {
                                return Err(SamlError::SignatureWrappingDetected(format!(
                                    "duplicate ID attribute '{v}' detected; possible XML signature wrapping attack"
                                )));
                            }
                            parsed_attrs.push((k, v));
                        }
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
                    } else if local_name == "Response" && is_protocol_ns && element_stack.is_empty()
                    {
                        // Why: bind envelope fields only to the root enclosing `<samlp:Response>`
                        // so a nested `<samlp:Response>` inside extension content cannot overwrite
                        // `Destination`, `InResponseTo`, or `response_id`.
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
                        // `<Assertion>` or the root `<Response>` and bind the capture to its
                        // opening stack depth so nested markup or comments cannot corrupt it.
                        match parent {
                            Some(("Assertion", XmlNs::Assertion)) => {
                                active_capture = Some(ActiveTextCapture {
                                    target: TextTarget::AssertionIssuer,
                                    depth: new_depth,
                                    buf: String::new(),
                                    attr_name: None,
                                });
                            }
                            Some(("Response", XmlNs::Protocol)) if element_stack.len() == 1 => {
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
                        // claims (`Conditions`, `AuthnStatement`, `Subject`/`NameID`,
                        // `SubjectConfirmationData`, `Audience`, `Attribute`, `AttributeValue`)
                        // to their schema positions under `<Assertion>` so extension containers
                        // such as `<Advice>` cannot inject forged claims.
                        if local_name == "AuthnStatement"
                            && matches!(parent, Some(("Assertion", XmlNs::Assertion)))
                        {
                            // Why: SAML 2.0 Core §2.7.2 requires both an `AuthnInstant`
                            // attribute and a child `<AuthnContext>` element before an
                            // `<AuthnStatement>` counts as valid authentication evidence.
                            current_authn_stmt_has_instant = false;
                            for (k, v) in &parsed_attrs {
                                if k == "AuthnInstant" {
                                    let _ = parse_iso8601_strict_parts(v)?;
                                    current_authn_stmt_has_instant = true;
                                }
                            }
                        } else if matches!(
                            local_name,
                            "AuthnContextClassRef" | "AuthnContextDecl" | "AuthnContextDeclRef"
                        ) && matches!(parent, Some(("AuthnContext", XmlNs::Assertion)))
                            && matches!(grandparent, Some(("AuthnStatement", XmlNs::Assertion)))
                            && matches!(great_grandparent, Some(("Assertion", XmlNs::Assertion)))
                        {
                            // Why: SAML Core §2.7.2.2 requires `<AuthnContext>` to carry at least
                            // one of `AuthnContextClassRef`, `AuthnContextDecl`, or
                            // `AuthnContextDeclRef`; an empty `<AuthnContext/>` is not valid
                            // authentication context evidence.
                            if current_authn_stmt_has_instant {
                                has_authn_statement = true;
                            }
                        } else if local_name == "Conditions"
                            && matches!(parent, Some(("Assertion", XmlNs::Assertion)))
                        {
                            if has_conditions {
                                return Err(SamlError::Malformed(
                                    "duplicate <Conditions> element in SAML Assertion".into(),
                                ));
                            }
                            has_conditions = true;
                            for (k, v) in &parsed_attrs {
                                if k == "NotBefore" {
                                    let (secs, nanos) = parse_iso8601_strict_parts(v)?;
                                    not_before = Some(secs);
                                    not_before_subsec_nanos = nanos;
                                } else if k == "NotOnOrAfter" {
                                    let (secs, nanos) = parse_iso8601_strict_parts(v)?;
                                    not_on_or_after = Some(secs);
                                    not_on_or_after_subsec_nanos = nanos;
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
                                        let (secs, nanos) = parse_iso8601_strict_parts(v)?;
                                        scd.not_on_or_after = Some(secs);
                                        scd.not_on_or_after_subsec_nanos = nanos;
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

                    let enters_signature =
                        in_signature || (local_name == "Signature" && is_dsig_ns);
                    element_stack.push((local_name.to_string(), ns, enters_signature));
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
                    } else if local_name == "AuthnStatement" && is_ns(&res, ASSERTION) {
                        current_authn_stmt_has_instant = false;
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
                Ok((_res, Event::Eof)) => {
                    if !element_stack.is_empty() {
                        return Err(SamlError::Malformed(
                            "unexpected EOF with unclosed XML elements".into(),
                        ));
                    }
                    break;
                }
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
            has_authn_statement,
            subject_confirmations,
            not_before,
            not_before_subsec_nanos,
            not_on_or_after,
            not_on_or_after_subsec_nanos,
            attributes: fields.attributes,
            signature_verified: signed_mode,
        })
    }
}
