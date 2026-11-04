//! Human-readable duration formatting and parsing.

use std::time::Duration;
use thiserror::Error;

/// Errors that can occur when parsing a human-readable duration string.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ParseDurationError {
    /// The input string was empty or contained only whitespace.
    #[error("input duration string is empty")]
    Empty,

    /// The numeric part could not be parsed.
    #[error("invalid numeric value in duration string: '{0}'")]
    InvalidNumber(String),

    /// The unit specified is unrecognized.
    #[error("unknown or invalid duration unit: '{0}'")]
    UnknownUnit(String),

    /// The computed duration exceeded `Duration::MAX`.
    #[error("duration value overflowed")]
    Overflow,
}

/// Formats a `Duration` into a clean, human-readable string.
///
/// # Examples
///
/// ```
/// use std::time::Duration;
/// use rume::helpers::format_duration;
///
/// assert_eq!(format_duration(Duration::from_millis(150)), "150ms");
/// assert_eq!(format_duration(Duration::from_secs(45)), "45s");
/// assert_eq!(format_duration(Duration::from_secs(90)), "1m 30s");
/// assert_eq!(format_duration(Duration::from_secs(3600)), "1h");
/// assert_eq!(format_duration(Duration::from_secs(86400 * 2)), "2d");
/// ```
pub fn format_duration(d: Duration) -> String {
    if d.is_zero() {
        return "0ms".to_string();
    }

    let total_secs = d.as_secs();
    let millis = d.subsec_millis();

    if total_secs == 0 {
        if millis == 0 {
            let micros = d.subsec_micros();
            if micros > 0 {
                return format!("{micros}µs");
            }
            return format!("{}ns", d.subsec_nanos());
        }
        return format!("{millis}ms");
    }

    let days = total_secs / 86400;
    let hours = (total_secs % 86400) / 3600;
    let mins = (total_secs % 3600) / 60;
    let secs = total_secs % 60;

    let mut parts = Vec::new();

    if days > 0 {
        parts.push(format!("{days}d"));
    }
    if hours > 0 {
        parts.push(format!("{hours}h"));
    }
    if mins > 0 {
        parts.push(format!("{mins}m"));
    }
    if secs > 0 || (days == 0 && hours == 0 && mins == 0) {
        if days == 0 && hours == 0 && mins == 0 && millis > 0 {
            let val = (secs as f64) + (millis as f64 / 1000.0);
            let formatted = format!("{val:.2}");
            let trimmed = formatted.trim_end_matches('0').trim_end_matches('.');
            return format!("{trimmed}s");
        }
        if secs > 0 {
            parts.push(format!("{secs}s"));
        }
    }

    parts.join(" ")
}

/// Parses a duration string (e.g. `"500ms"`, `"15s"`, `"10m"`, `"2h"`, `"7d"`, or `"1m 30s"`) into a `Duration`.
///
/// Supports fractional seconds (`"1.5s"`) and compound expressions (`"1h 15m"`).
///
/// # Examples
///
/// ```
/// use std::time::Duration;
/// use rume::helpers::parse_duration;
///
/// assert_eq!(parse_duration("500ms").unwrap(), Duration::from_millis(500));
/// assert_eq!(parse_duration("15s").unwrap(), Duration::from_secs(15));
/// assert_eq!(parse_duration("1.5s").unwrap(), Duration::from_millis(1500));
/// assert_eq!(parse_duration("1m 30s").unwrap(), Duration::from_secs(90));
/// ```
pub fn parse_duration(input: &str) -> Result<Duration, ParseDurationError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(ParseDurationError::Empty);
    }

    let mut total_duration = Duration::ZERO;
    let mut remainder = trimmed;

    while !remainder.is_empty() {
        remainder = remainder.trim_start();
        if remainder.is_empty() {
            break;
        }

        // Find boundary between number and unit
        let num_end = remainder
            .find(|ch: char| !ch.is_ascii_digit() && ch != '.')
            .unwrap_or(remainder.len());

        let (num_str, after_num) = remainder.split_at(num_end);
        let num_str = num_str.trim();

        if num_str.is_empty() {
            return Err(ParseDurationError::InvalidNumber(remainder.to_string()));
        }

        let after_num = after_num.trim_start();
        // Unit ends at whitespace or at next digit
        let unit_end = after_num
            .find(|ch: char| ch.is_whitespace() || ch.is_ascii_digit())
            .unwrap_or(after_num.len());

        let (unit_str, next_remainder) = after_num.split_at(unit_end);
        let unit_str = unit_str.trim().to_ascii_lowercase();

        let part_dur = parse_single_duration_token(num_str, &unit_str)?;
        total_duration = total_duration
            .checked_add(part_dur)
            .ok_or(ParseDurationError::Overflow)?;

        remainder = next_remainder;
    }

    Ok(total_duration)
}

fn parse_single_duration_token(
    num_str: &str,
    unit_str: &str,
) -> Result<Duration, ParseDurationError> {
    let nanos_per_unit: u128 = match unit_str {
        "ns" | "nanos" | "nanosecond" | "nanoseconds" => 1,
        "us" | "µs" | "micros" | "microsecond" | "microseconds" => 1_000,
        "ms" | "millis" | "millisecond" | "milliseconds" => 1_000_000,
        "s" | "sec" | "secs" | "second" | "seconds" => 1_000_000_000,
        "m" | "min" | "mins" | "minute" | "minutes" => 60 * 1_000_000_000,
        "h" | "hr" | "hrs" | "hour" | "hours" => 3600 * 1_000_000_000,
        "d" | "day" | "days" => 86400 * 1_000_000_000,
        _ => return Err(ParseDurationError::UnknownUnit(unit_str.to_string())),
    };

    if num_str.contains('.') {
        let val: f64 = num_str
            .parse()
            .map_err(|_| ParseDurationError::InvalidNumber(num_str.to_string()))?;

        if val < 0.0 || val.is_nan() || val.is_infinite() {
            return Err(ParseDurationError::InvalidNumber(num_str.to_string()));
        }

        let total_nanos = val * (nanos_per_unit as f64);
        if total_nanos > (u64::MAX as f64 * 1_000_000_000.0) {
            return Err(ParseDurationError::Overflow);
        }

        Ok(Duration::from_nanos(total_nanos.round() as u64))
    } else {
        let val: u64 = num_str
            .parse()
            .map_err(|_| ParseDurationError::InvalidNumber(num_str.to_string()))?;

        match (val as u128).checked_mul(nanos_per_unit) {
            Some(nanos) if nanos <= u64::MAX as u128 => Ok(Duration::from_nanos(nanos as u64)),
            _ => {
                let secs_multiplier = nanos_per_unit / 1_000_000_000;
                if secs_multiplier > 0 {
                    let secs = val
                        .checked_mul(secs_multiplier as u64)
                        .ok_or(ParseDurationError::Overflow)?;
                    Ok(Duration::from_secs(secs))
                } else {
                    Err(ParseDurationError::Overflow)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_duration_various() {
        assert_eq!(format_duration(Duration::ZERO), "0ms");
        assert_eq!(format_duration(Duration::from_nanos(750)), "750ns");
        assert_eq!(format_duration(Duration::from_micros(50)), "50µs");
        assert_eq!(format_duration(Duration::from_millis(500)), "500ms");
        assert_eq!(format_duration(Duration::from_millis(1500)), "1.5s");
        assert_eq!(format_duration(Duration::from_secs(30)), "30s");
        assert_eq!(format_duration(Duration::from_secs(90)), "1m 30s");
        assert_eq!(format_duration(Duration::from_secs(3600)), "1h");
        assert_eq!(format_duration(Duration::from_secs(3665)), "1h 1m 5s");
        assert_eq!(format_duration(Duration::from_secs(86400 * 3)), "3d");
    }

    #[test]
    fn parse_duration_simple() {
        assert_eq!(parse_duration("500ms").unwrap(), Duration::from_millis(500));
        assert_eq!(parse_duration("10s").unwrap(), Duration::from_secs(10));
        assert_eq!(parse_duration("15m").unwrap(), Duration::from_secs(15 * 60));
        assert_eq!(parse_duration("2h").unwrap(), Duration::from_secs(2 * 3600));
        assert_eq!(
            parse_duration("7d").unwrap(),
            Duration::from_secs(7 * 86400)
        );
    }

    #[test]
    fn parse_duration_compound_and_decimals() {
        assert_eq!(parse_duration("1.5s").unwrap(), Duration::from_millis(1500));
        assert_eq!(parse_duration("1m 30s").unwrap(), Duration::from_secs(90));
        assert_eq!(
            parse_duration("2h 15m 10s").unwrap(),
            Duration::from_secs(2 * 3600 + 15 * 60 + 10)
        );
    }

    #[test]
    fn parse_duration_errors() {
        assert_eq!(parse_duration(""), Err(ParseDurationError::Empty));
        assert!(matches!(
            parse_duration("xyz"),
            Err(ParseDurationError::InvalidNumber(_))
        ));
        assert!(matches!(
            parse_duration("10 weeks"),
            Err(ParseDurationError::UnknownUnit(_))
        ));
    }
}
