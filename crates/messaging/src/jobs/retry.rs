//! Retry policies and backoff computation.

use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Retry strategy configuration for background jobs.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RetryPolicy {
    /// Maximum number of execution attempts before marking the job as dead.
    pub max_attempts: u32,
    /// Initial backoff delay in milliseconds.
    pub initial_backoff_ms: u64,
    /// Maximum backoff delay cap in milliseconds.
    pub max_backoff_ms: u64,
    /// Exponential multiplier applied on each consecutive failure.
    pub backoff_factor: f64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            initial_backoff_ms: 1000, // 1s
            max_backoff_ms: 60_000,   // 60s
            backoff_factor: 2.0,
        }
    }
}

impl RetryPolicy {
    /// Creates a default retry policy with 3 attempts.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the maximum number of attempts.
    pub fn with_max_attempts(mut self, attempts: u32) -> Self {
        self.max_attempts = attempts;
        self
    }

    /// Sets the initial backoff duration.
    pub fn with_initial_backoff(mut self, duration: Duration) -> Self {
        self.initial_backoff_ms = u64::try_from(duration.as_millis()).unwrap_or(u64::MAX);
        self
    }

    /// Sets the maximum backoff duration cap.
    pub fn with_max_backoff(mut self, duration: Duration) -> Self {
        self.max_backoff_ms = u64::try_from(duration.as_millis()).unwrap_or(u64::MAX);
        self
    }

    /// Sets the exponential backoff multiplier.
    pub fn with_backoff_factor(mut self, factor: f64) -> Self {
        self.backoff_factor = factor;
        self
    }

    /// Computes the delay before the next retry attempt.
    ///
    /// Returns `None` if `attempt >= max_attempts`.
    pub fn next_backoff(&self, attempt: u32) -> Option<Duration> {
        if attempt >= self.max_attempts {
            return None;
        }

        let factor = if self.backoff_factor.is_finite() && self.backoff_factor >= 1.0 {
            self.backoff_factor
        } else {
            2.0
        };

        // Why: saturating `i32::try_from` prevents `attempt > i32::MAX as u32 + 1`
        // from wrapping negative in `as i32`, which would invert `powi` to ~0ms
        // instead of saturating at `max_backoff_ms`.
        let exponent = i32::try_from(attempt.saturating_sub(1)).unwrap_or(i32::MAX);
        let factor_pow = factor.powi(exponent);
        let backoff_ms = if factor_pow.is_finite() {
            let scaled = self.initial_backoff_ms as f64 * factor_pow;
            if scaled.is_finite() && scaled >= 0.0 && scaled < (u64::MAX as f64) {
                scaled.round() as u64
            } else {
                self.max_backoff_ms
            }
        } else {
            self.max_backoff_ms
        };
        let capped_ms = backoff_ms.min(self.max_backoff_ms);

        Some(Duration::from_millis(capped_ms))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_backoff_saturates_on_huge_attempt_and_duration() {
        let policy = RetryPolicy::new()
            .with_max_attempts(u32::MAX)
            .with_initial_backoff(Duration::from_millis(100))
            .with_max_backoff(Duration::from_secs(60));
        assert_eq!(
            policy.next_backoff(i32::MAX as u32 + 2),
            Some(Duration::from_secs(60))
        );
        assert_eq!(
            policy.next_backoff(u32::MAX - 1),
            Some(Duration::from_secs(60))
        );
    }
}
