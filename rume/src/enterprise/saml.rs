//! SAML 2.0 assertion parsing, claim extraction, and condition validation.

use std::collections::BTreeMap;
use thiserror::Error;

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

    /// Missing mandatory digital signature verifier.
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
}

/// Interface for verifying cryptographic signatures on SAML assertions.
pub trait SamlSignatureVerifier: Send + Sync {
    /// Verifies the digital signature on the raw XML assertion.
    fn verify_signature(&self, raw_xml: &str) -> Result<(), SamlError>;
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

/// Parser and validator for SAML 2.0 assertions.
pub struct SamlParser;

fn strip_comments(xml: &str) -> Result<String, SamlError> {
    let mut out = String::with_capacity(xml.len());
    let mut rest = xml;
    while let Some(start) = rest.find("<!--") {
        out.push_str(&rest[..start]);
        if let Some(end) = rest[start..].find("-->") {
            rest = &rest[start + end + 3..];
        } else {
            return Err(SamlError::Malformed("unclosed XML comment".into()));
        }
    }
    out.push_str(rest);
    Ok(out)
}

fn parse_iso8601_or_epoch(s: &str) -> Option<u64> {
    if let Ok(epoch) = s.trim().parse::<u64>() {
        return Some(epoch);
    }
    let s = s.trim().trim_end_matches('Z');
    let (date, time) = s.split_once('T')?;
    let mut date_parts = date.split('-');
    let year: u64 = date_parts.next()?.parse().ok()?;
    let month: u64 = date_parts.next()?.parse().ok()?;
    let day: u64 = date_parts.next()?.parse().ok()?;

    let mut time_parts = time.split(':');
    let hour: u64 = time_parts.next()?.parse().ok()?;
    let min: u64 = time_parts.next()?.parse().ok()?;
    let sec_str = time_parts.next()?;
    let sec: u64 = sec_str.split('.').next()?.parse().ok()?;

    if year < 1970 || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }

    let mut days: u64 = 0;
    for y in 1970..year {
        let is_leap = (y.is_multiple_of(4) && !y.is_multiple_of(100)) || y.is_multiple_of(400);
        days += if is_leap { 366 } else { 365 };
    }
    let is_leap_year =
        (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400);
    let days_in_months = [
        31,
        if is_leap_year { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    for m in 1..month {
        days += days_in_months[(m - 1) as usize];
    }
    days += day - 1;

    Some(days * 86400 + hour * 3600 + min * 60 + sec)
}

fn extract_attribute(tag_slice: &str, attr: &str) -> Option<String> {
    for quote in ['"', '\''] {
        let pattern_exact = format!("{attr}={quote}");
        let pattern_space1 = format!("{attr} ={quote}");
        let pattern_space2 = format!("{attr} = {quote}");
        let pattern_space3 = format!("{attr}= {quote}");
        for p in [&pattern_exact, &pattern_space1, &pattern_space2, &pattern_space3] {
            if let Some(pos) = tag_slice.find(p) {
                let rest = &tag_slice[pos + p.len()..];
                if let Some(end) = rest.find(quote) {
                    return Some(rest[..end].to_string());
                }
            }
        }
    }
    None
}

impl SamlParser {
    fn extract_tag<'a>(xml: &'a str, tag: &str) -> Option<&'a str> {
        let close_tag = format!("</{tag}>");
        let tag_with_gt = format!("<{tag}>");
        let tag_with_space = format!("<{tag} ");

        let content_start = if let Some(pos) = xml.find(&tag_with_gt) {
            pos + tag_with_gt.len()
        } else if let Some(pos) = xml.find(&tag_with_space) {
            let rest = &xml[pos..];
            let tag_close = rest.find('>')?;
            pos + tag_close + 1
        } else {
            return None;
        };

        let end = xml[content_start..].find(&close_tag)?;
        Some(xml[content_start..content_start + end].trim())
    }

    /// Parses an XML string containing a SAML 2.0 Assertion.
    ///
    /// Deprecated: Unverified assertion parsing in production is insecure.
    /// Prefer [`SamlParser::parse_signed`] with an injected [`SamlSignatureVerifier`].
    #[deprecated(note = "use SamlParser::parse_signed() in production or SamlParser::parse_unverified() for tests")]
    pub fn parse(xml: &str) -> Result<SamlAssertion, SamlError> {
        Self::parse_unverified(xml)
    }

    /// Parses an XML string containing a SAML 2.0 Assertion without signature verification.
    /// Explicitly intended for offline tests and diagnostics.
    pub fn parse_unverified(xml: &str) -> Result<SamlAssertion, SamlError> {
        Self::parse_with_verifier(xml, None)
    }

    /// Parses an XML string containing a SAML 2.0 Assertion with mandatory cryptographic signature verification.
    pub fn parse_signed(
        xml: &str,
        verifier: &dyn SamlSignatureVerifier,
    ) -> Result<SamlAssertion, SamlError> {
        Self::parse_with_verifier(xml, Some(verifier))
    }

    /// Parses an XML string containing a SAML 2.0 Assertion and verifies its digital signature if a verifier is provided.
    pub fn parse_with_verifier(
        xml: &str,
        verifier: Option<&dyn SamlSignatureVerifier>,
    ) -> Result<SamlAssertion, SamlError> {
        if let Some(v) = verifier {
            v.verify_signature(xml)?;
        }

        let clean_xml = strip_comments(xml)?;
        let xml = clean_xml.as_str();

        // Security check: Guard against XML Signature Wrapping (XSW) attacks
        // Detect all assertion elements regardless of namespace prefix (<([a-zA-Z0-9_\-]+:)?Assertion)
        let mut assertion_positions = Vec::new();
        let mut search_idx = 0;
        while let Some(pos) = xml[search_idx..].find(":Assertion") {
            let abs_pos = search_idx + pos;
            if let Some(tag_start) = xml[..abs_pos].rfind('<') {
                let tag_prefix = &xml[tag_start + 1..abs_pos];
                if tag_prefix.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-') {
                    assertion_positions.push(tag_start);
                }
            }
            search_idx = abs_pos + ":Assertion".len();
        }
        search_idx = 0;
        while let Some(pos) = xml[search_idx..].find("<Assertion") {
            let abs_pos = search_idx + pos;
            if !assertion_positions.contains(&abs_pos) {
                assertion_positions.push(abs_pos);
            }
            search_idx = abs_pos + "<Assertion".len();
        }

        if assertion_positions.len() > 1 {
            return Err(SamlError::SignatureWrappingDetected(format!(
                "multiple ({}) Assertion elements detected; possible XML signature wrapping attack",
                assertion_positions.len()
            )));
        }

        if assertion_positions.is_empty() {
            return Err(SamlError::Malformed("missing Assertion element in SAML payload".into()));
        }

        let assertion_start = assertion_positions[0];
        let assertion_xml = if let Some(close_pos) = xml[assertion_start..].find(":Assertion>") {
            if let Some(_close_tag_start) = xml[assertion_start..assertion_start + close_pos].rfind("</") {
                let end = assertion_start + close_pos + ":Assertion>".len();
                &xml[assertion_start..end]
            } else {
                &xml[assertion_start..]
            }
        } else if let Some(close_pos) = xml[assertion_start..].find("</Assertion>") {
            let end = assertion_start + close_pos + "</Assertion>".len();
            &xml[assertion_start..end]
        } else {
            &xml[assertion_start..]
        };

        // Extract Assertion ID and enclosing Response ID using robust attribute parsing
        let assertion_opening_tag = assertion_xml.split('>').next().unwrap_or(assertion_xml);
        let assertion_id = extract_attribute(assertion_opening_tag, "ID");
        let id = assertion_id.clone().unwrap_or_else(|| "assertion".to_string());

        let response_id = {
            let mut resp_id = None;
            for token in [":Response", "<Response"] {
                if let Some(pos) = xml.find(token) {
                    let start = xml[..pos].rfind('<').unwrap_or(pos);
                    if let Some(tag_end) = xml[start..].find('>') {
                        let tag = &xml[start..start + tag_end + 1];
                        if let Some(id) = extract_attribute(tag, "ID") {
                            resp_id = Some(id);
                            break;
                        }
                    }
                }
            }
            resp_id
        };

        // Security check: Signature Reference URI must match Assertion ID or enclosing Response ID
        let mut search_idx = 0;
        while let Some(pos) = xml[search_idx..].find('<') {
            let tag_start = search_idx + pos;
            let tag_content = &xml[tag_start..];
            let Some(tag_end) = tag_content.find('>') else {
                break;
            };
            let tag_slice = &tag_content[..tag_end + 1];
            let is_ref = tag_slice.starts_with("<ds:Reference")
                || tag_slice.starts_with("<Reference")
                || tag_slice.starts_with("<dsig:Reference");

            if is_ref
                && let Some(uri) = extract_attribute(tag_slice, "URI")
                && let Some(ref_id) = uri.strip_prefix('#')
            {
                let matches_assertion = assertion_id.as_deref() == Some(ref_id);
                let matches_response = response_id.as_deref() == Some(ref_id);
                if !matches_assertion && !matches_response {
                    let expected = match (&assertion_id, &response_id) {
                        (Some(a), Some(r)) => {
                            format!("assertion ID '{a}' or response ID '{r}'")
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
            search_idx = tag_start + tag_end + 1;
        }

        // Find Issuer inside isolated assertion subtree
        let issuer = Self::extract_tag(assertion_xml, "saml:Issuer")
            .or_else(|| Self::extract_tag(assertion_xml, "saml2:Issuer"))
            .or_else(|| Self::extract_tag(assertion_xml, "Issuer"))
            .ok_or_else(|| SamlError::Malformed("missing Issuer element".into()))?
            .to_string();

        // Find Subject NameID inside isolated assertion subtree (prevents Envelope Injection XSW)
        let subject = Self::extract_tag(assertion_xml, "saml:NameID")
            .or_else(|| Self::extract_tag(assertion_xml, "saml2:NameID"))
            .or_else(|| Self::extract_tag(assertion_xml, "NameID"))
            .ok_or_else(|| SamlError::Malformed("missing NameID in Subject".into()))?
            .to_string();

        // Optional Audience inside isolated assertion subtree
        let audience = Self::extract_tag(assertion_xml, "saml:Audience")
            .or_else(|| Self::extract_tag(assertion_xml, "saml2:Audience"))
            .or_else(|| Self::extract_tag(assertion_xml, "Audience"))
            .map(str::to_string);

        // Optional Destination endpoint
        let destination = if let Some(pos) = xml.find("Destination=\"") {
            let rest = &xml[pos + "Destination=\"".len()..];
            rest.find('"').map(|end| rest[..end].to_string())
        } else {
            None
        };

        // Parse Conditions NotBefore and NotOnOrAfter inside isolated assertion subtree
        let mut not_before = None;
        let mut not_on_or_after = None;

        let cond_tags = ["<saml2:Conditions", "<saml:Conditions", "<Conditions"];
        for cond_tag in cond_tags {
            if let Some(pos) = assertion_xml.find(cond_tag) {
                let rest = &assertion_xml[pos..];
                if let Some(close_bracket) = rest.find('>') {
                    let tag_slice = &rest[..close_bracket];
                    if let Some(nb) = extract_attribute(tag_slice, "NotBefore") {
                        not_before = parse_iso8601_or_epoch(&nb);
                    }
                    if let Some(noa) = extract_attribute(tag_slice, "NotOnOrAfter") {
                        not_on_or_after = parse_iso8601_or_epoch(&noa);
                    }
                }
                break;
            }
        }

        // Extract Attributes inside isolated assertion subtree
        let mut attributes: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut search_idx = 0;

        while let Some(pos) = assertion_xml[search_idx..].find("<saml:Attribute")
            .or_else(|| assertion_xml[search_idx..].find("<saml2:Attribute"))
            .or_else(|| assertion_xml[search_idx..].find("<Attribute"))
        {
            let tag_start = search_idx + pos;
            let Some(tag_end) = assertion_xml[tag_start..].find('>') else { break };
            let tag_header = &assertion_xml[tag_start..tag_start + tag_end];
            let attr_body_start = tag_start + tag_end + 1;

            if let Some(name) = extract_attribute(tag_header, "Name") {
                if let Some(close_attr) = assertion_xml[attr_body_start..]
                    .find("</saml:Attribute>")
                    .or_else(|| assertion_xml[attr_body_start..].find("</saml2:Attribute>"))
                    .or_else(|| assertion_xml[attr_body_start..].find("</Attribute>"))
                {
                    let attr_content = &assertion_xml[attr_body_start..attr_body_start + close_attr];
                    let mut vals = Vec::new();
                    let mut content_slice = attr_content;
                    while let Some(open_idx) = content_slice.find("AttributeValue") {
                        let rest = &content_slice[open_idx..];
                        let Some(val_tag_end) = rest.find('>') else { break };
                        let after_tag = &rest[val_tag_end + 1..];
                        let Some(close_idx) = after_tag.find("</") else {
                            break;
                        };
                        let val = after_tag[..close_idx].trim().to_string();
                        vals.push(val);
                        let after_close = &after_tag[close_idx..];
                        let Some(close_tag_end) = after_close.find('>') else {
                            break;
                        };
                        content_slice = &after_close[close_tag_end + 1..];
                    }

                    attributes.insert(name, vals);
                    search_idx = attr_body_start + close_attr;
                } else {
                    search_idx = attr_body_start;
                }
            } else {
                search_idx = attr_body_start;
            }
        }

        Ok(SamlAssertion {
            id,
            issuer,
            subject,
            audience,
            destination,
            not_before,
            not_on_or_after,
            attributes,
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

        if let Some(not_on_or_after) = assertion.not_on_or_after
            && current_epoch_sec >= not_on_or_after
        {
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
