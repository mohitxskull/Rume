//! Route parameter matchers.
//!
//! Provides the [`Matcher`] enum used with [`crate::routing::RouteSpec::where_param`]
//! to validate route parameters such as `:id`, `:slug`, etc.

use std::fmt::Debug;

/// A route parameter matcher.
///
/// Used with [`crate::routing::RouteSpec::where_param`] to validate and cast
/// route parameters.
///
/// # Matchers
///
/// - [`Matcher::Number`] — digits only; returns the original string (not a
///   parsed number). Overflowing digit strings (e.g. u64::MAX+1) are rejected.
/// - [`Matcher::Uuid`] — UUID format (8-4-4-4-12 hex chars); returns lowercase.
/// - [`Matcher::Slug`] — URL-friendly slug; returns the string as-is.
#[derive(Debug, Clone)]
pub enum Matcher {
    /// Matches digits only, returns the string as-is.
    ///
    /// Validation: the entire value must parse as `u64`. Returns the original
    /// string, not a parsed number.
    Number,
    /// Matches UUID format (8-4-4-4-12 hex chars), returns lowercase `String`.
    Uuid,
    /// Matches URL-friendly slugs, returns the string as-is.
    Slug,
}

impl Matcher {
    /// Validate a string value against this matcher.
    ///
    /// Returns the validated string on success, or a generic error message on
    /// failure. The error message does NOT echo the raw input value to avoid
    /// leaking user-controlled data into error responses.
    pub fn validate(&self, value: &str) -> Result<String, String> {
        match self {
            Matcher::Number => {
                if value.parse::<u64>().is_ok() {
                    Ok(value.to_string())
                } else {
                    Err("value is not a valid number".to_string())
                }
            }
            Matcher::Uuid => {
                if let Ok(parsed) = uuid::Uuid::parse_str(value) {
                    Ok(parsed.to_string())
                } else {
                    Err("value is not a valid UUID".to_string())
                }
            }
            Matcher::Slug => {
                if is_valid_slug(value) {
                    Ok(value.to_string())
                } else {
                    Err("value is not a valid slug".to_string())
                }
            }
        }
    }
}

fn is_valid_slug(value: &str) -> bool {
    if value.len() < 3 {
        return false;
    }
    if value.contains(' ') {
        return false;
    }
    if value.starts_with('-') || value.starts_with('_') {
        return false;
    }
    if value.ends_with('-') || value.ends_with('_') {
        return false;
    }
    if value.contains("--") || value.contains("__") {
        return false;
    }
    value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matcher_number_validates_digits() {
        assert_eq!(Matcher::Number.validate("42"), Ok("42".to_string()));
        assert!(Matcher::Number.validate("abc").is_err());
        // u64 overflow should reject
        assert!(Matcher::Number.validate("99999999999999999999").is_err());
    }

    #[test]
    fn matcher_uuid_validates_format() {
        assert_eq!(
            Matcher::Uuid.validate("550e8400-e29b-41d4-a716-446655440000"),
            Ok("550e8400-e29b-41d4-a716-446655440000".to_string())
        );
        assert!(Matcher::Uuid.validate("not-a-uuid").is_err());
    }

    #[test]
    fn matcher_slug_validates_format() {
        assert_eq!(Matcher::Slug.validate("my-slug"), Ok("my-slug".to_string()));
        assert!(Matcher::Slug.validate("bad slug").is_err());
        assert!(Matcher::Slug.validate("ab").is_err()); // too short
        assert!(Matcher::Slug.validate("-bad").is_err()); // leading hyphen
        assert!(Matcher::Slug.validate("bad--").is_err()); // doubled hyphen
    }
}
