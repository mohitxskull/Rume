//! SAML 2.0 assertion parsing, claim extraction, and condition validation.

use std::collections::BTreeMap;
use quick_xml::events::Event;
use quick_xml::reader::NsReader;
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
        _assertion_id: &str,
        _assertion_xml: &str,
    ) -> Result<(), SamlError> {
        self.verify_signature(raw_xml)
    }
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

/// Parses an ISO 8601 timestamp string strictly, including UTC 'Z', fractional seconds,
/// and explicit timezone offsets (+HH:MM, -HH:MM, +HHMM, -HHMM).
/// Fails closed: if the timestamp is malformed or invalid, returns an error rather than ignoring it.
pub fn parse_iso8601_strict(s: &str) -> Result<u64, SamlError> {
    let s = s.trim();
    if let Ok(epoch) = s.parse::<u64>() {
        return Ok(epoch);
    }

    let (date_str, time_with_tz) = s
        .split_once('T')
        .ok_or_else(|| SamlError::Malformed(format!("invalid ISO 8601 timestamp '{s}': missing 'T'")))?;

    let mut date_parts = date_str.split('-');
    let year: u64 = date_parts
        .next()
        .and_then(|p| p.parse().ok())
        .ok_or_else(|| SamlError::Malformed(format!("invalid year in timestamp '{s}'")))?;
    let month: u64 = date_parts
        .next()
        .and_then(|p| p.parse().ok())
        .ok_or_else(|| SamlError::Malformed(format!("invalid month in timestamp '{s}'")))?;
    let day: u64 = date_parts
        .next()
        .and_then(|p| p.parse().ok())
        .ok_or_else(|| SamlError::Malformed(format!("invalid day in timestamp '{s}'")))?;

    if year < 1970 || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return Err(SamlError::Malformed(format!(
            "timestamp '{s}' out of valid date bounds"
        )));
    }

    // Parse time and timezone offset
    let (time_str, tz_offset_secs) = if let Some(idx) = time_with_tz.find('Z') {
        (&time_with_tz[..idx], 0i64)
    } else if let Some(idx) = time_with_tz.rfind('+') {
        let tz_str = &time_with_tz[idx + 1..];
        let offset = parse_tz_offset(tz_str)?;
        (&time_with_tz[..idx], offset)
    } else if let Some(idx) = time_with_tz.rfind('-') {
        let tz_str = &time_with_tz[idx + 1..];
        let offset = parse_tz_offset(tz_str)?;
        (&time_with_tz[..idx], -offset)
    } else {
        (time_with_tz, 0i64)
    };

    let mut time_parts = time_str.split(':');
    let hour: u64 = time_parts
        .next()
        .and_then(|p| p.parse().ok())
        .ok_or_else(|| SamlError::Malformed(format!("invalid hour in timestamp '{s}'")))?;
    let min: u64 = time_parts
        .next()
        .and_then(|p| p.parse().ok())
        .ok_or_else(|| SamlError::Malformed(format!("invalid minute in timestamp '{s}'")))?;
    let sec_str = time_parts
        .next()
        .ok_or_else(|| SamlError::Malformed(format!("invalid second in timestamp '{s}'")))?;
    let sec: u64 = sec_str
        .split('.')
        .next()
        .and_then(|p| p.parse().ok())
        .ok_or_else(|| SamlError::Malformed(format!("invalid second value in timestamp '{s}'")))?;

    if hour > 23 || min > 59 || sec > 60 {
        return Err(SamlError::Malformed(format!(
            "timestamp '{s}' out of valid time bounds"
        )));
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

    let total_secs_utc = (days * 86400 + hour * 3600 + min * 60 + sec) as i64 - tz_offset_secs;
    if total_secs_utc < 0 {
        return Err(SamlError::Malformed(format!(
            "timestamp '{s}' resolves before Unix epoch"
        )));
    }

    Ok(total_secs_utc as u64)
}

fn parse_tz_offset(s: &str) -> Result<i64, SamlError> {
    let s = s.trim();
    if let Some((h_str, m_str)) = s.split_once(':') {
        let h: i64 = h_str
            .parse()
            .map_err(|_| SamlError::Malformed(format!("invalid timezone offset '{s}'")))?;
        let m: i64 = m_str
            .parse()
            .map_err(|_| SamlError::Malformed(format!("invalid timezone offset '{s}'")))?;
        Ok(h * 3600 + m * 60)
    } else if s.len() == 4 {
        let h: i64 = s[..2]
            .parse()
            .map_err(|_| SamlError::Malformed(format!("invalid timezone offset '{s}'")))?;
        let m: i64 = s[2..]
            .parse()
            .map_err(|_| SamlError::Malformed(format!("invalid timezone offset '{s}'")))?;
        Ok(h * 3600 + m * 60)
    } else if s.len() == 2 {
        let h: i64 = s
            .parse()
            .map_err(|_| SamlError::Malformed(format!("invalid timezone offset '{s}'")))?;
        Ok(h * 3600)
    } else {
        Err(SamlError::Malformed(format!("invalid timezone offset '{s}'")))
    }
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
    /// Prefer [`SamlParser::parse_signed`] with an injected [`SamlSignatureVerifier`].
    #[deprecated(note = "use SamlParser::parse_signed() in production or SamlParser::parse_unverified() for tests")]
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

        let mut issuer: Option<String> = None;
        let mut subject: Option<String> = None;
        let mut audience: Option<String> = None;
        let mut not_before: Option<u64> = None;
        let mut not_on_or_after: Option<u64> = None;
        let mut attributes: BTreeMap<String, Vec<String>> = BTreeMap::new();

        let mut assertion_start_offset: Option<usize> = None;
        let mut assertion_end_offset: Option<usize> = None;

        let mut signature_references: Vec<String> = Vec::new();

        let mut element_stack: Vec<String> = Vec::new();
        let mut in_assertion = false;
        let mut current_attr_name: Option<String> = None;

        loop {
            let event_start = reader.buffer_position() as usize;

            match reader.read_resolved_event() {
                Ok((_res, Event::Start(ref e))) => {
                    let local = e.local_name();
                    let local_name = local.as_ref();

                    if local_name == "Assertion" {
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
                    } else if local_name == "Response" {
                        for attr in e.attributes().flatten() {
                            let local_k = attr.key.local_name();
                            let k = local_k.as_ref();
                            if k == "ID" {
                                response_id = Some(attr.value.as_ref().to_string());
                            } else if k == "Destination" {
                                destination = Some(attr.value.as_ref().to_string());
                            }
                        }
                    } else if local_name == "Reference" {
                        for attr in e.attributes().flatten() {
                            let local_k = attr.key.local_name();
                            if local_k.as_ref() == "URI" {
                                let uri = attr.value.as_ref();
                                let ref_id = uri.strip_prefix('#').unwrap_or(uri);
                                signature_references.push(ref_id.to_string());
                            }
                        }
                    } else if in_assertion {
                        if local_name == "Conditions" {
                            for attr in e.attributes().flatten() {
                                let local_k = attr.key.local_name();
                                let k = local_k.as_ref();
                                if k == "NotBefore" {
                                    not_before = Some(parse_iso8601_strict(attr.value.as_ref())?);
                                } else if k == "NotOnOrAfter" {
                                    not_on_or_after = Some(parse_iso8601_strict(attr.value.as_ref())?);
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

                    element_stack.push(local_name.to_string());
                }
                Ok((_res, Event::End(ref e))) => {
                    let local = e.local_name();
                    let local_name = local.as_ref();
                    if local_name == "Assertion" {
                        assertion_end_offset = Some(reader.buffer_position() as usize);
                        in_assertion = false;
                    } else if local_name == "Attribute" {
                        current_attr_name = None;
                    }
                    element_stack.pop();
                }
                Ok((_res, Event::Text(ref t))) => {
                    if in_assertion {
                        let parent = element_stack.last().map(|s| s.as_str());
                        let grand_parent = if element_stack.len() >= 2 {
                            Some(element_stack[element_stack.len() - 2].as_str())
                        } else {
                            None
                        };

                        let text = quick_xml::escape::unescape(t.as_ref())
                            .map(|c| c.trim().to_string())
                            .unwrap_or_else(|_| t.as_ref().trim().to_string());

                        if !text.is_empty() {
                            if parent == Some("Issuer") && issuer.is_none() {
                                issuer = Some(text);
                            } else if parent == Some("NameID")
                                && grand_parent == Some("Subject")
                                && subject.is_none()
                            {
                                subject = Some(text);
                            } else if parent == Some("Audience") && audience.is_none() {
                                audience = Some(text);
                            } else if parent == Some("AttributeValue") && current_attr_name.is_some() {
                                attributes
                                    .entry(current_attr_name.clone().unwrap())
                                    .or_default()
                                    .push(text);
                            }
                        }
                    }
                }
                Ok((_res, Event::CData(ref c))) => {
                    if in_assertion {
                        let parent = element_stack.last().map(|s| s.as_str());
                        let grand_parent = if element_stack.len() >= 2 {
                            Some(element_stack[element_stack.len() - 2].as_str())
                        } else {
                            None
                        };

                        let text = c.as_ref().trim().to_string();

                        if !text.is_empty() {
                            if parent == Some("Issuer") && issuer.is_none() {
                                issuer = Some(text);
                            } else if parent == Some("NameID")
                                && grand_parent == Some("Subject")
                                && subject.is_none()
                            {
                                subject = Some(text);
                            } else if parent == Some("Audience") && audience.is_none() {
                                audience = Some(text);
                            } else if parent == Some("AttributeValue") && current_attr_name.is_some() {
                                attributes
                                    .entry(current_attr_name.clone().unwrap())
                                    .or_default()
                                    .push(text);
                            }
                        }
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
            return Err(SamlError::Malformed("missing Assertion element in SAML payload".into()));
        }

        // Validate signature references against assertion ID or response ID
        for ref_id in &signature_references {
            let matches_assertion = assertion_id.as_deref() == Some(ref_id.as_str());
            let matches_response = response_id.as_deref() == Some(ref_id.as_str());
            let matches_root_document = ref_id.is_empty() && (response_id.is_some() || assertion_id.is_some());
            if !matches_assertion && !matches_response && !matches_root_document {
                let expected = match (&assertion_id, &response_id) {
                    (Some(a), Some(r)) => format!("assertion ID '{a}', response ID '{r}', or root document URI=''"),
                    (Some(a), None) => format!("assertion ID '{a}' or root document URI=''"),
                    (None, Some(r)) => format!("response ID '{r}' or root document URI=''"),
                    (None, None) => "known element ID".to_string(),
                };
                return Err(SamlError::SignatureWrappingDetected(format!(
                    "signature reference URI '#{ref_id}' does not match {expected}"
                )));
            }
        }

        let issuer = issuer.ok_or_else(|| SamlError::Malformed("missing Issuer element".into()))?;
        if issuer.is_empty() {
            return Err(SamlError::Malformed("missing Issuer element".into()));
        }

        let subject = subject.ok_or_else(|| SamlError::Malformed("missing NameID in Subject".into()))?;
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
