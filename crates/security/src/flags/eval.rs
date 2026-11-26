//! Deterministic feature flag evaluation engine.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Targeting override rule for a [`FeatureFlag`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TargetingRule {
    /// Enable the flag unconditionally for the given tenant ID.
    Tenant(String),
    /// Enable the flag unconditionally for the given user ID.
    User(String),
}

/// Percentage rollout in `0..=100`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RolloutPercentage(pub u8);

impl RolloutPercentage {
    /// Creates a rollout percentage clamped to `0..=100`.
    pub fn new(percentage: u8) -> Self {
        Self(percentage.min(100))
    }

    /// Returns the clamped percentage in `0..=100`.
    pub fn value(self) -> u8 {
        self.0.min(100)
    }
}

impl Default for RolloutPercentage {
    fn default() -> Self {
        Self(100)
    }
}

/// Feature flag definition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeatureFlag {
    /// Unique flag key (e.g. `"new_checkout"`).
    pub key: String,
    /// Optional tenant scope; `None` denotes a global flag.
    pub tenant_id: Option<String>,
    /// Master kill-switch. When `false`, evaluation always returns `false`.
    pub enabled: bool,
    /// Deterministic rollout percentage (`0..=100`).
    pub rollout: RolloutPercentage,
    /// Explicit tenant/user allow-list rules evaluated before percentage rollout.
    pub rules: Vec<TargetingRule>,
}

impl FeatureFlag {
    /// Creates a new global feature flag with 100% rollout and no targeting rules.
    pub fn new(key: impl Into<String>, enabled: bool) -> Self {
        Self {
            key: key.into(),
            tenant_id: None,
            enabled,
            rollout: RolloutPercentage::default(),
            rules: Vec::new(),
        }
    }

    /// Scopes this flag to a specific tenant.
    pub fn with_tenant_id(mut self, tenant_id: impl Into<String>) -> Self {
        self.tenant_id = Some(tenant_id.into());
        self
    }

    /// Sets the rollout percentage (clamped to `0..=100`).
    pub fn with_rollout(mut self, percentage: u8) -> Self {
        self.rollout = RolloutPercentage::new(percentage);
        self
    }

    /// Appends a targeting rule.
    pub fn with_rule(mut self, rule: TargetingRule) -> Self {
        self.rules.push(rule);
        self
    }

    /// Replaces the targeting rule list.
    pub fn with_rules(mut self, rules: Vec<TargetingRule>) -> Self {
        self.rules = rules;
        self
    }
}

/// Subject context passed to [`evaluate_flag`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EvaluationContext {
    /// Active tenant ID, if any.
    pub tenant_id: Option<String>,
    /// Active user ID, if any.
    pub user_id: Option<String>,
}

impl EvaluationContext {
    /// Creates an empty evaluation context.
    pub fn new() -> Self {
        Self::default()
    }

    /// Attaches a tenant ID.
    pub fn with_tenant(mut self, tenant_id: impl Into<String>) -> Self {
        self.tenant_id = Some(tenant_id.into());
        self
    }

    /// Attaches a user ID.
    pub fn with_user(mut self, user_id: impl Into<String>) -> Self {
        self.user_id = Some(user_id.into());
        self
    }
}

/// Computes a deterministic bucket in `0..100` using SHA-256 over
/// `"{flag_key}:{subject_id}"`.
///
/// Why: `sha2::Sha256` is already pinned in `rume-security` and yields
/// identical, cross-process, cross-restart bucketing without introducing an
/// extra hash dependency.
pub fn bucket_percentage(flag_key: &str, subject_id: &str) -> u8 {
    let mut hasher = Sha256::new();
    hasher.update(flag_key.as_bytes());
    hasher.update(b":");
    hasher.update(subject_id.as_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    (u64::from_be_bytes(bytes) % 100) as u8
}

/// Evaluates `flag` for `ctx`.
///
/// Order of precedence:
/// 1. If `!flag.enabled`, returns `false`.
/// 2. If any [`TargetingRule::Tenant`] or [`TargetingRule::User`] matches `ctx`, returns `true`.
/// 3. Evaluates [`RolloutPercentage`] deterministically using `ctx.user_id`
///    (falling back to `ctx.tenant_id`) via [`bucket_percentage`].
pub fn evaluate_flag(flag: &FeatureFlag, ctx: &EvaluationContext) -> bool {
    if !flag.enabled {
        return false;
    }

    for rule in &flag.rules {
        match rule {
            TargetingRule::Tenant(tid) if ctx.tenant_id.as_deref() == Some(tid.as_str()) => {
                return true;
            }
            TargetingRule::User(uid) if ctx.user_id.as_deref() == Some(uid.as_str()) => {
                return true;
            }
            _ => {}
        }
    }

    let pct = flag.rollout.value();
    if pct >= 100 {
        return true;
    }
    if pct == 0 {
        return false;
    }

    let Some(subject_id) = ctx.user_id.as_deref().or(ctx.tenant_id.as_deref()) else {
        return false;
    };

    bucket_percentage(&flag.key, subject_id) < pct
}
