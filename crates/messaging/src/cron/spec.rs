//! Cron job specification, timezone evaluation, and missed-tick policy.

use chrono::{DateTime, FixedOffset, Utc};
use croner::Cron;
use croner::parser::{CronParser, Seconds};
use thiserror::Error;

/// Errors returned by [`crate::cron::CronScheduler`] and [`CronJobSpec`].
#[derive(Debug, Error)]
pub enum CronError {
    /// Invalid cron expression syntax.
    #[error("invalid cron expression '{expression}': {message}")]
    InvalidExpression { expression: String, message: String },
    /// Scheduler execution or handler failure.
    #[error("cron execution error: {0}")]
    Execution(String),
}

/// Policy governing how missed cron ticks are handled after leadership failover or delay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MissedTickPolicy {
    /// Skip any missed ticks and schedule the next future occurrence.
    #[default]
    Skip,
    /// Execute a single catch-up tick immediately, then resume normal scheduling.
    RunOnce,
}

/// Result of evaluating a scheduled cron tick against the current wall-clock time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TickDecision {
    /// Whether the job should execute now.
    pub should_run: bool,
    /// Next scheduled occurrence in UTC.
    pub next_scheduled: DateTime<Utc>,
}

/// Specification for a recurring cron job.
#[derive(Debug, Clone)]
pub struct CronJobSpec {
    /// Unique name identifying this cron job.
    pub name: String,
    /// 5-field or 6-field cron expression.
    pub expression: String,
    /// Timezone offset for evaluating the cron schedule (defaults to UTC).
    pub timezone: FixedOffset,
    /// Policy for missed ticks.
    pub missed_tick_policy: MissedTickPolicy,
    cron: Cron,
}

impl CronJobSpec {
    /// Creates a new UTC cron job specification, validating a 5-field or 6-field cron expression.
    pub fn new(name: impl Into<String>, expression: impl Into<String>) -> Result<Self, CronError> {
        let name = name.into();
        let expression = expression.into();
        let cron = CronParser::builder()
            .seconds(Seconds::Optional)
            .build()
            .parse(&expression)
            .map_err(|err| CronError::InvalidExpression {
                expression: expression.clone(),
                message: err.to_string(),
            })?;

        Ok(Self {
            name,
            expression,
            timezone: FixedOffset::east_opt(0).expect("UTC offset 0 is always valid"),
            missed_tick_policy: MissedTickPolicy::default(),
            cron,
        })
    }

    /// Sets a fixed timezone offset for evaluating the cron schedule.
    pub fn with_timezone(mut self, timezone: FixedOffset) -> Self {
        self.timezone = timezone;
        self
    }

    /// Sets the [`MissedTickPolicy`] for this cron job.
    pub fn with_missed_tick_policy(mut self, policy: MissedTickPolicy) -> Self {
        self.missed_tick_policy = policy;
        self
    }

    /// Computes the next occurrence strictly after `after` in the configured timezone,
    /// returning the timestamp in UTC.
    pub fn next_tick(&self, after: DateTime<Utc>) -> Result<DateTime<Utc>, CronError> {
        let localized = after.with_timezone(&self.timezone);
        let next = self
            .cron
            .find_next_occurrence(&localized, false)
            .map_err(|err| CronError::Execution(err.to_string()))?;
        Ok(next.with_timezone(&Utc))
    }

    /// Evaluates whether `scheduled_at` is due or missed at `now`, applying [`MissedTickPolicy`].
    pub fn evaluate_tick(
        &self,
        scheduled_at: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> Result<TickDecision, CronError> {
        if now < scheduled_at {
            return Ok(TickDecision {
                should_run: false,
                next_scheduled: scheduled_at,
            });
        }

        let next_after = self.next_tick(scheduled_at)?;
        if now < next_after {
            return Ok(TickDecision {
                should_run: true,
                next_scheduled: next_after,
            });
        }

        let next_future = self.next_tick(now)?;
        let should_run = matches!(self.missed_tick_policy, MissedTickPolicy::RunOnce);
        Ok(TickDecision {
            should_run,
            next_scheduled: next_future,
        })
    }
}
