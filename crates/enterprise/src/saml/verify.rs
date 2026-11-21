use super::assertion::{SamlAssertion, SamlParser};
use std::collections::HashMap;
use std::sync::Mutex;
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

impl SamlParser {
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
