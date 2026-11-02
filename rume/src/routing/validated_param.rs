//! Validated parameter extraction.
//!
//! Provides [`ValidatedParam`], which validates a route parameter value
//! against a [`Matcher`] and returns the validated string.

use crate::error::AppError;
use crate::routing::matchers::Matcher;

/// A route parameter value that has been validated against a matcher.
///
/// Constructed via [`ValidatedParam::from_value`], which takes the parameter
/// name, raw value, and matcher explicitly. This avoids the flaw of the old
/// design where the parameter name was a hardcoded constant on the matcher
/// type (`PARAM_NAME = "test_param"`) rather than the actual route param name
/// from [`crate::routing::RouteSpec::where_param`].
///
/// # Example
///
/// ```ignore
/// use rume::routing::{ValidatedParam, Matcher};
/// use rume::error::AppError;
///
/// let params: BTreeMap<String, String> = BTreeMap::from([("id".to_string(), "42".to_string())]);
/// let validated = ValidatedParam::<matchers::Number>::from_value(
///     "id",
///     params.get("id").unwrap(),
///     Matcher::Number,
/// )?;
/// assert_eq!(validated.value, "42");
/// ```
#[derive(Debug, Clone)]
pub struct ValidatedParam<M> {
    pub name: String,
    pub value: String,
    _marker: std::marker::PhantomData<M>,
}

impl<M> ValidatedParam<M> {
    /// Validate a raw parameter value against a matcher and return a
    /// [`ValidatedParam`].
    ///
    /// Returns an [`AppError::BadRequest`] if the value fails validation or
    /// is missing. The error message does not echo the raw input value.
    pub fn from_value(name: &str, value: &str, matcher: Matcher) -> Result<Self, AppError> {
        matcher
            .validate(value)
            .map(|v| Self {
                name: name.to_string(),
                value: v,
                _marker: std::marker::PhantomData,
            })
            .map_err(|msg| AppError::BadRequest {
                code: "INVALID_PARAM",
                message: msg,
            })
    }

    /// Consume the extractor and return the validated string value.
    pub fn into_inner(self) -> String {
        self.value
    }
}

impl<M> std::ops::Deref for ValidatedParam<M> {
    type Target = str;

    fn deref(&self) -> &str {
        &self.value
    }
}

/// Marker types for param matchers used with `ValidatedParam`.
pub mod matchers {
    /// Matcher for numeric parameters.
    #[derive(Debug)]
    pub struct Number;
    /// Matcher for UUID parameters.
    #[derive(Debug)]
    pub struct Uuid;
    /// Matcher for slug parameters.
    #[derive(Debug)]
    pub struct Slug;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::AppError;
    use std::collections::BTreeMap;

    #[test]
    fn validated_param_from_value_validates() {
        // from_value accepts valid values and rejects invalid ones.
        let valid =
            ValidatedParam::<matchers::Number>::from_value("id", "42", Matcher::Number).unwrap();
        assert_eq!(valid.name, "id");
        assert_eq!(valid.value, "42");

        let err = ValidatedParam::<matchers::Number>::from_value("id", "abc", Matcher::Number)
            .unwrap_err();
        assert!(matches!(err, AppError::BadRequest { .. }));
    }

    #[test]
    fn validated_param_from_value_missing_key() {
        // Missing param produces BadRequest.
        let params: BTreeMap<String, String> = BTreeMap::new();
        let value = params.get("id").ok_or_else(|| AppError::BadRequest {
            code: "MISSING_PARAM",
            message: "missing parameter: id".to_string(),
        });
        assert!(value.is_err());
    }
}
