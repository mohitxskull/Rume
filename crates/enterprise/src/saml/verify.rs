use super::assertion::{SamlAssertion, SamlParser};
use super::redis_replay::AsyncSamlReplayCache;
use std::collections::{BTreeSet, HashMap};
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
    /// Maximum clock skew (in seconds) that validators sharing this cache may configure.
    ///
    /// Why: when multiple validators share one replay cache with different `clock_skew_secs`,
    /// every record must be retained for at least `not_on_or_after + max_clock_skew_secs()` so
    /// a validator with smaller skew cannot cause premature eviction before a validator with
    /// larger skew stops accepting the assertion.
    fn max_clock_skew_secs(&self) -> u64 {
        300
    }

    /// Atomically checks expiry and records `assertion_id` with acceptance deadline `expires_at`
    /// (epoch seconds) at `now_epoch_sec` (epoch seconds).
    ///
    /// Implementations must enforce expiry under the same synchronization and time basis as
    /// pruning: if `now_epoch_sec >= expires_at` (or the cache has already pruned past
    /// `expires_at`), return `Err(SamlError::Expired { .. })` instead of inserting a stale ID.
    /// Returns `Ok(true)` if fresh (newly recorded), or `Ok(false)` if already seen.
    fn record_if_absent(
        &self,
        assertion_id: &str,
        expires_at: u64,
        now_epoch_sec: u64,
    ) -> Result<bool, SamlError>;
}

#[derive(Debug, Default)]
struct ReplayCacheState {
    entries: HashMap<String, u64>,
    /// Secondary expiry index `(expires_at, assertion_id)` so pruning expired entries
    /// is `O(k log N)` for `k` expired entries instead of an `O(N)` full-map scan.
    expiry_index: BTreeSet<(u64, String)>,
    last_pruned_at: u64,
}

impl ReplayCacheState {
    fn prune_up_to(&mut self, cutoff: u64) {
        self.last_pruned_at = self.last_pruned_at.max(cutoff);
        let watermark = self.last_pruned_at;
        while let Some((exp, _)) = self.expiry_index.first() {
            if *exp > watermark {
                break;
            }
            if let Some((exp, id)) = self.expiry_index.pop_first()
                && self.entries.get(&id).copied() == Some(exp)
            {
                self.entries.remove(&id);
            }
        }
    }
}

const DEFAULT_MAX_REPLAY_ENTRIES: usize = 100_000;

/// Thread-safe in-memory [`SamlReplayCache`] suitable for single-instance deployments and tests.
#[derive(Debug)]
pub struct InMemorySamlReplayCache {
    state: Mutex<ReplayCacheState>,
    max_clock_skew_secs: u64,
    max_entries: usize,
}

impl Default for InMemorySamlReplayCache {
    fn default() -> Self {
        Self {
            state: Mutex::new(ReplayCacheState::default()),
            max_clock_skew_secs: 300,
            max_entries: DEFAULT_MAX_REPLAY_ENTRIES,
        }
    }
}

impl InMemorySamlReplayCache {
    /// Creates a new empty replay cache with the default maximum clock skew (300 seconds).
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a new empty replay cache enforcing `max_clock_skew_secs` across all validators
    /// sharing this cache.
    pub fn with_max_clock_skew(max_clock_skew_secs: u64) -> Self {
        Self {
            state: Mutex::new(ReplayCacheState::default()),
            max_clock_skew_secs,
            max_entries: DEFAULT_MAX_REPLAY_ENTRIES,
        }
    }

    /// Configures the maximum number of active assertion IDs retained before failing closed.
    pub fn with_max_entries(mut self, max_entries: usize) -> Self {
        self.max_entries = max_entries.max(1);
        self
    }

    /// Evicts entries whose expiration is `<= now_epoch_sec` and advances the pruning
    /// watermark so concurrent late validations cannot re-insert an already-pruned ID.
    pub fn prune_expired(&self, now_epoch_sec: u64) {
        if let Ok(mut guard) = self.state.lock() {
            guard.prune_up_to(now_epoch_sec);
        }
    }
}

impl SamlReplayCache for InMemorySamlReplayCache {
    fn max_clock_skew_secs(&self) -> u64 {
        self.max_clock_skew_secs
    }

    fn record_if_absent(
        &self,
        assertion_id: &str,
        expires_at: u64,
        now_epoch_sec: u64,
    ) -> Result<bool, SamlError> {
        let mut guard = self
            .state
            .lock()
            .map_err(|e| SamlError::Malformed(format!("replay cache lock poisoned: {e}")))?;
        // Why: enforce the acceptance deadline under the same mutex and monotonic watermark
        // as `prune_expired` so a validation that passed the earlier time check right before
        // expiry cannot race with pruning and re-insert an evicted assertion ID.
        let effective_now = now_epoch_sec.max(guard.last_pruned_at);
        if effective_now >= expires_at {
            return Err(SamlError::Expired {
                not_on_or_after: expires_at,
                current: effective_now,
            });
        }
        // Why: advance `last_pruned_at` to `effective_now` before evicting expired entries so
        // an evicted ID can never be re-accepted by a lagging/concurrent validation with an
        // earlier `now_epoch_sec`, and prune via `expiry_index` in O(k log N) instead of an
        // O(N) scan; fail closed at `max_entries` so unbounded traffic cannot OOM or evict
        // active replay state.
        guard.prune_up_to(effective_now);
        if guard.entries.contains_key(assertion_id) {
            return Ok(false);
        }
        if guard.entries.len() >= self.max_entries {
            return Err(SamlError::Malformed(format!(
                "SAML replay cache capacity ({}) exceeded",
                self.max_entries
            )));
        }
        let id = assertion_id.to_string();
        guard.entries.insert(id.clone(), expires_at);
        guard.expiry_index.insert((expires_at, id));
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

    /// Validates a SAML assertion against the full SAML 2.0 HTTP POST Web Browser SSO profile.
    ///
    /// In addition to every check performed by [`Self::validate_with_config`], this entry point
    /// mandates:
    /// - cryptographic signature verification (`assertion.is_signature_verified()`),
    /// - a valid `<AuthnStatement>` with `AuthnInstant` and `<AuthnContext>` (`assertion.has_authn_statement`),
    /// - bearer `<SubjectConfirmation>` enforcement (`require_bearer_confirmation: true`),
    /// - a configured SP audience (`config.expected_audience.is_some()`),
    /// - a configured ACS URL (`config.expected_recipient.is_some()`), and
    /// - a replay-protection cache (`config.replay_cache.is_some()`).
    pub fn validate_browser_sso(
        assertion: &SamlAssertion,
        current_epoch_sec: u64,
        config: &SamlValidationConfig<'_>,
    ) -> Result<(), SamlError> {
        if !assertion.is_signature_verified() {
            return Err(SamlError::MissingSignatureVerifier);
        }
        if !assertion.has_authn_statement {
            return Err(SamlError::Malformed(
                "missing required AuthnStatement (with AuthnInstant and AuthnContext) for SAML Web Browser SSO"
                    .into(),
            ));
        }
        if config.expected_audience.is_none() {
            return Err(SamlError::Malformed(
                "expected_audience (SP entity ID) is required for SAML Web Browser SSO validation"
                    .into(),
            ));
        }
        if config.expected_issuer.is_none() {
            return Err(SamlError::Malformed(
                "expected_issuer (IdP entity ID) is required for SAML Web Browser SSO validation"
                    .into(),
            ));
        }
        if config.expected_recipient.is_none() {
            return Err(SamlError::Malformed(
                "expected_recipient (ACS URL) is required for SAML Web Browser SSO validation"
                    .into(),
            ));
        }
        if config.replay_cache.is_none() {
            return Err(SamlError::Malformed(
                "replay_cache is required for HTTP POST SAML Web Browser SSO validation".into(),
            ));
        }
        // Why: SAML 2.0 HTTP POST Binding §3.5.5.2 requires verifying `<Response Destination="...">`
        // whenever present; default to `expected_recipient` (ACS URL) when `expected_destination`
        // was not explicitly configured (`RC-14`).
        let effective_destination = config.expected_destination.or_else(|| {
            assertion
                .destination
                .as_ref()
                .and(config.expected_recipient)
        });
        let enforced = SamlValidationConfig {
            expected_destination: effective_destination,
            require_bearer_confirmation: true,
            ..*config
        };
        Self::validate_with_config(assertion, current_epoch_sec, &enforced)
    }

    /// Validates configured SAML assertion fields (time conditions with subsecond precision
    /// and clock skew, multi-audience restrictions, optional issuer/destination/recipient/
    /// `InResponseTo`, bearer `SubjectConfirmationData` when `require_bearer_confirmation` is
    /// `true`, and optional replay protection).
    ///
    /// Note: this method checks only the fields configured in `config` and does **not** require
    /// `<AuthnStatement>`, `expected_recipient`, or `replay_cache` unless configured. For full
    /// SAML 2.0 HTTP POST Web Browser SSO enforcement, call [`Self::validate_browser_sso`].
    pub fn validate_with_config(
        assertion: &SamlAssertion,
        current_epoch_sec: u64,
        config: &SamlValidationConfig<'_>,
    ) -> Result<(), SamlError> {
        let skew = config.clock_skew_secs;

        if let Some(not_before) = assertion.not_before
            && (current_epoch_sec.saturating_add(skew), 0u32)
                < (not_before, assertion.not_before_subsec_nanos)
        {
            return Err(SamlError::NotYetValid {
                not_before,
                current: current_epoch_sec,
            });
        }

        let not_on_or_after = assertion
            .not_on_or_after
            .ok_or(SamlError::MissingExpirationCondition)?;

        if (current_epoch_sec, 0u32)
            >= (
                not_on_or_after.saturating_add(skew),
                assertion.not_on_or_after_subsec_nanos,
            )
        {
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
                if (current_epoch_sec, 0u32)
                    >= (
                        scd_exp.saturating_add(skew),
                        scd.not_on_or_after_subsec_nanos,
                    )
                {
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
            let max_skew = cache.max_clock_skew_secs();
            if skew > max_skew {
                return Err(SamlError::Malformed(format!(
                    "validator clock_skew_secs ({skew}) exceeds replay cache max_clock_skew_secs ({max_skew})"
                )));
            }
            // Why: retain the replay record through the cache's full `max_clock_skew_secs`
            // window (rounding up fractional nanoseconds) and pass `current_epoch_sec` so a
            // validator with smaller skew cannot cause premature eviction before a peer
            // validator with larger skew stops accepting the assertion, and pruning stays
            // atomic with acceptance.
            let replay_expires_at = not_on_or_after
                .saturating_add(max_skew)
                .saturating_add(u64::from(assertion.not_on_or_after_subsec_nanos > 0));
            let fresh =
                cache.record_if_absent(&assertion.id, replay_expires_at, current_epoch_sec)?;
            if !fresh {
                return Err(SamlError::AssertionReplayed(assertion.id.clone()));
            }
        }

        Ok(())
    }

    /// Validates a SAML assertion using an asynchronous replay-protection cache.
    pub async fn validate_with_async_replay(
        assertion: &SamlAssertion,
        current_epoch_sec: u64,
        config: &SamlValidationConfig<'_>,
        cache: &dyn AsyncSamlReplayCache,
    ) -> Result<(), SamlError> {
        let sync_cfg = SamlValidationConfig {
            replay_cache: None,
            ..*config
        };
        Self::validate_with_config(assertion, current_epoch_sec, &sync_cfg)?;

        let not_on_or_after = assertion
            .not_on_or_after
            .ok_or(SamlError::MissingExpirationCondition)?;
        let max_skew = cache.max_clock_skew_secs();
        if config.clock_skew_secs > max_skew {
            return Err(SamlError::Malformed(format!(
                "validator clock_skew_secs ({}) exceeds replay cache max_clock_skew_secs ({max_skew})",
                config.clock_skew_secs
            )));
        }
        let replay_expires_at = not_on_or_after
            .saturating_add(max_skew)
            .saturating_add(u64::from(assertion.not_on_or_after_subsec_nanos > 0));
        let fresh = cache
            .record_if_absent_async(&assertion.id, replay_expires_at, current_epoch_sec)
            .await?;
        if !fresh {
            return Err(SamlError::AssertionReplayed(assertion.id.clone()));
        }
        Ok(())
    }

    /// Validates a SAML assertion against the full SAML 2.0 HTTP POST Web Browser SSO profile
    /// using an asynchronous replay-protection cache (e.g. [`super::RedisSamlReplayCache`]).
    pub async fn validate_browser_sso_async(
        assertion: &SamlAssertion,
        current_epoch_sec: u64,
        config: &SamlValidationConfig<'_>,
        cache: &dyn AsyncSamlReplayCache,
    ) -> Result<(), SamlError> {
        if !assertion.is_signature_verified() {
            return Err(SamlError::MissingSignatureVerifier);
        }
        if !assertion.has_authn_statement {
            return Err(SamlError::Malformed(
                "missing required AuthnStatement (with AuthnInstant and AuthnContext) for SAML Web Browser SSO"
                    .into(),
            ));
        }
        if config.expected_audience.is_none() {
            return Err(SamlError::Malformed(
                "expected_audience (SP entity ID) is required for SAML Web Browser SSO validation"
                    .into(),
            ));
        }
        if config.expected_issuer.is_none() {
            return Err(SamlError::Malformed(
                "expected_issuer (IdP entity ID) is required for SAML Web Browser SSO validation"
                    .into(),
            ));
        }
        if config.expected_recipient.is_none() {
            return Err(SamlError::Malformed(
                "expected_recipient (ACS URL) is required for SAML Web Browser SSO validation"
                    .into(),
            ));
        }
        let effective_destination = config.expected_destination.or_else(|| {
            assertion
                .destination
                .as_ref()
                .and(config.expected_recipient)
        });
        let enforced = SamlValidationConfig {
            expected_destination: effective_destination,
            require_bearer_confirmation: true,
            ..*config
        };
        Self::validate_with_async_replay(assertion, current_epoch_sec, &enforced, cache).await
    }
}

#[cfg(test)]
mod replay_cache_tests {
    use super::*;

    #[test]
    fn in_memory_replay_cache_prunes_expired_inline_and_enforces_max_entries() {
        let cache = InMemorySamlReplayCache::new().with_max_entries(2);
        assert!(cache.record_if_absent("id-1", 100, 50).unwrap());
        assert!(cache.record_if_absent("id-2", 200, 50).unwrap());

        // At capacity (2 active entries): existing replay still returns Ok(false).
        assert!(!cache.record_if_absent("id-1", 100, 60).unwrap());
        // New unexpired entry fails closed when full.
        assert!(matches!(
            cache.record_if_absent("id-3", 200, 60),
            Err(SamlError::Malformed(ref msg)) if msg.contains("capacity")
        ));

        // Once id-1 expires (now >= 100), inline pruning frees its slot for id-3.
        assert!(cache.record_if_absent("id-3", 250, 100).unwrap());
    }

    #[test]
    fn in_memory_replay_cache_advances_watermark_on_inline_prune() {
        let cache = InMemorySamlReplayCache::new();
        assert!(cache.record_if_absent("id-a", 120, 100).unwrap());
        // Advances time past id-a's expiry (120) and evicts id-a inline.
        assert!(cache.record_if_absent("id-b", 200, 125).unwrap());
        // A lagging/concurrent validation with an earlier `now_epoch_sec` must NOT
        // re-accept the evicted id-a; it must fail closed with Expired.
        assert_eq!(
            cache.record_if_absent("id-a", 120, 115),
            Err(SamlError::Expired {
                not_on_or_after: 120,
                current: 125,
            })
        );
    }
}
