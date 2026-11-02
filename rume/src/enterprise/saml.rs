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
    /// NotBefore epoch seconds condition.
    pub not_before: Option<u64>,
    /// NotOnOrAfter epoch seconds condition.
    pub not_on_or_after: Option<u64>,
    /// Extracted user claims and attributes.
    pub attributes: BTreeMap<String, Vec<String>>,
}

/// Parser and validator for SAML 2.0 assertions.
pub struct SamlParser;

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
    pub fn parse(xml: &str) -> Result<SamlAssertion, SamlError> {
        // Find Issuer
        let issuer = Self::extract_tag(xml, "saml:Issuer")
            .or_else(|| Self::extract_tag(xml, "saml2:Issuer"))
            .or_else(|| Self::extract_tag(xml, "Issuer"))
            .ok_or_else(|| SamlError::Malformed("missing Issuer element".into()))?
            .to_string();

        // Find Subject NameID
        let subject = Self::extract_tag(xml, "saml:NameID")
            .or_else(|| Self::extract_tag(xml, "saml2:NameID"))
            .or_else(|| Self::extract_tag(xml, "NameID"))
            .ok_or_else(|| SamlError::Malformed("missing NameID in Subject".into()))?
            .to_string();

        // Optional Audience
        let audience = Self::extract_tag(xml, "saml:Audience")
            .or_else(|| Self::extract_tag(xml, "saml2:Audience"))
            .or_else(|| Self::extract_tag(xml, "Audience"))
            .map(str::to_string);

        // Optional Assertion ID
        let id = if let Some(pos) = xml.find("ID=\"") {
            let rest = &xml[pos + 4..];
            if let Some(end) = rest.find('"') {
                rest[..end].to_string()
            } else {
                "assertion".to_string()
            }
        } else {
            "assertion".to_string()
        };

        // Extract Attributes
        let mut attributes: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let attr_pattern = "Attribute Name=\"";
        let mut search_idx = 0;

        while let Some(pos) = xml[search_idx..].find(attr_pattern) {
            let start = search_idx + pos + attr_pattern.len();
            if let Some(end) = xml[start..].find('"') {
                let name = &xml[start..start + end];
                let attr_body_start = start + end;

                if let Some(close_attr) = xml[attr_body_start..]
                    .find("</saml:Attribute>")
                    .or_else(|| xml[attr_body_start..].find("</saml2:Attribute>"))
                    .or_else(|| xml[attr_body_start..].find("</Attribute>"))
                {
                    let attr_content = &xml[attr_body_start..attr_body_start + close_attr];
                    let mut vals = Vec::new();
                    let mut content_slice = attr_content;
                    while let Some(open_idx) = content_slice.find("AttributeValue") {
                        let rest = &content_slice[open_idx..];
                        let Some(tag_end) = rest.find('>') else { break };
                        let after_tag = &rest[tag_end + 1..];
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

                    attributes.insert(name.to_string(), vals);
                    search_idx = attr_body_start + close_attr;
                } else {
                    search_idx = attr_body_start;
                }
            } else {
                break;
            }
        }

        Ok(SamlAssertion {
            id,
            issuer,
            subject,
            audience,
            not_before: None,
            not_on_or_after: None,
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
}
