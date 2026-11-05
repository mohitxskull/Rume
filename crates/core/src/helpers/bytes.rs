//! Byte size formatting and parsing.

use std::num::IntErrorKind;
use thiserror::Error;

const KIB: u64 = 1024;
const MIB: u64 = 1024 * 1024;
const GIB: u64 = 1024 * 1024 * 1024;
const TIB: u64 = 1024 * 1024 * 1024 * 1024;

/// Errors that can occur when parsing a human-readable byte string.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ParseBytesError {
    /// The input string was empty or contained only whitespace.
    #[error("input byte string is empty")]
    Empty,

    /// The numeric part could not be parsed.
    #[error("invalid numeric value in byte string: '{0}'")]
    InvalidNumber(String),

    /// The unit specified is unrecognized.
    #[error("unknown or invalid byte unit: '{0}'")]
    UnknownUnit(String),

    /// The computed byte value exceeded the capacity of `u64`.
    #[error("byte value overflowed u64")]
    Overflow,
}

/// Formats a raw byte count into a human-readable binary string (`B`, `KiB`, `MiB`, `GiB`, `TiB`).
///
/// # Examples
///
/// ```
/// use rume::helpers::format_bytes;
///
/// assert_eq!(format_bytes(512), "512 B");
/// assert_eq!(format_bytes(1024), "1 KiB");
/// assert_eq!(format_bytes(1_572_864), "1.5 MiB");
/// assert_eq!(format_bytes(10_737_418_240), "10 GiB");
/// ```
pub fn format_bytes(bytes: u64) -> String {
    if bytes < KIB {
        return format!("{bytes} B");
    }

    let (unit, divisor) = if bytes >= TIB {
        ("TiB", TIB as f64)
    } else if bytes >= GIB {
        ("GiB", GIB as f64)
    } else if bytes >= MIB {
        ("MiB", MIB as f64)
    } else {
        ("KiB", KIB as f64)
    };

    let value = bytes as f64 / divisor;
    // Format up to 2 decimal places, omitting trailing zeros after decimal point
    let formatted = format!("{value:.2}");
    let trimmed = formatted.trim_end_matches('0').trim_end_matches('.');
    format!("{trimmed} {unit}")
}

/// Parses a human-readable byte string (e.g. `"10MB"`, `"500 KiB"`, `"1.5gb"`, `"1024"`) into bytes.
///
/// Supports binary units (`KiB`, `MiB`, `GiB`, `TiB`) and standard abbreviations (`B`, `K`, `KB`, `M`, `MB`, `G`, `GB`, `T`, `TB`).
/// In accordance with standard web configuration conventions, `KB`, `MB`, and `GB` map to powers of 1024.
///
/// # Examples
///
/// ```
/// use rume::helpers::parse_bytes;
///
/// assert_eq!(parse_bytes("1024").unwrap(), 1024);
/// assert_eq!(parse_bytes("500 B").unwrap(), 500);
/// assert_eq!(parse_bytes("10 MB").unwrap(), 10 * 1024 * 1024);
/// assert_eq!(parse_bytes("1.5 GiB").unwrap(), 1_610_612_736);
/// ```
pub fn parse_bytes(input: &str) -> Result<u64, ParseBytesError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(ParseBytesError::Empty);
    }

    let split_pos = trimmed
        .find(|ch: char| !ch.is_ascii_digit() && ch != '.')
        .unwrap_or(trimmed.len());

    let (num_str, unit_str) = trimmed.split_at(split_pos);
    let num_str = num_str.trim();
    let unit_str = unit_str.trim().to_ascii_lowercase();

    if num_str.is_empty() {
        return Err(ParseBytesError::InvalidNumber(trimmed.to_string()));
    }

    let multiplier = match unit_str.as_str() {
        "" | "b" | "byte" | "bytes" => 1u64,
        "k" | "kb" | "kib" => KIB,
        "m" | "mb" | "mib" => MIB,
        "g" | "gb" | "gib" => GIB,
        "t" | "tb" | "tib" => TIB,
        _ => return Err(ParseBytesError::UnknownUnit(unit_str)),
    };

    if num_str.contains('.') {
        let val: f64 = num_str
            .parse()
            .map_err(|_| ParseBytesError::InvalidNumber(num_str.to_string()))?;

        if val < 0.0 || val.is_nan() || val.is_infinite() {
            return Err(ParseBytesError::InvalidNumber(num_str.to_string()));
        }

        let total = val * (multiplier as f64);
        if total > u64::MAX as f64 {
            return Err(ParseBytesError::Overflow);
        }

        Ok(total.round() as u64)
    } else {
        let val: u64 = num_str.parse().map_err(|err: std::num::ParseIntError| {
            if *err.kind() == IntErrorKind::PosOverflow {
                ParseBytesError::Overflow
            } else {
                ParseBytesError::InvalidNumber(num_str.to_string())
            }
        })?;

        val.checked_mul(multiplier).ok_or(ParseBytesError::Overflow)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_bytes_correctly() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1024), "1 KiB");
        assert_eq!(format_bytes(1536), "1.5 KiB");
        assert_eq!(format_bytes(10 * 1024 * 1024), "10 MiB");
        assert_eq!(format_bytes(2 * 1024 * 1024 * 1024), "2 GiB");
    }

    #[test]
    fn parse_bytes_integers() {
        assert_eq!(parse_bytes("0").unwrap(), 0);
        assert_eq!(parse_bytes("512").unwrap(), 512);
        assert_eq!(parse_bytes("1024 B").unwrap(), 1024);
        assert_eq!(parse_bytes("10 KB").unwrap(), 10 * 1024);
        assert_eq!(parse_bytes("5mb").unwrap(), 5 * 1024 * 1024);
        assert_eq!(parse_bytes("2 GiB").unwrap(), 2 * 1024 * 1024 * 1024);
        assert_eq!(parse_bytes("1TB").unwrap(), 1024 * 1024 * 1024 * 1024);
    }

    #[test]
    fn parse_bytes_decimals() {
        assert_eq!(parse_bytes("1.5 KiB").unwrap(), 1536);
        assert_eq!(parse_bytes("0.5 MB").unwrap(), 524288);
    }

    #[test]
    fn parse_bytes_errors() {
        assert_eq!(parse_bytes(""), Err(ParseBytesError::Empty));
        assert!(matches!(
            parse_bytes("invalid"),
            Err(ParseBytesError::InvalidNumber(_))
        ));
        assert!(matches!(
            parse_bytes("10 xyz"),
            Err(ParseBytesError::UnknownUnit(_))
        ));
        assert_eq!(
            parse_bytes("999999999999999999999 TB"),
            Err(ParseBytesError::Overflow)
        );
    }
}
