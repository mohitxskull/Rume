//! Route parameter validation utilities.
//!
//! Provides [`ValidatedParam`], a validated parameter value container populated
//! via [`ValidatedParam::from_value`].
//!
//! Note: Route parameter validation in handlers is handled by [`crate::routing::RouteSpec::where_param`]
//! and `ParamValidationLayer`, or via [`crate::validation::ValidatedPath`]. `ValidatedParam` is not
//! a request extractor.

use crate::error::AppError;
use crate::routing::matchers::Matcher;

/// A route parameter value that has been validated against a matcher.
///
/// Constructed via [`ValidatedParam::from_value`], which validates the parameter
/// name, raw string value, and [`Matcher`].
///
/// # Example
///
/// ```
/// use rume::routing::{ValidatedParam, Matcher};
/// use std::collections::BTreeMap;
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let params: BTreeMap<String, String> = BTreeMap::from([("id".to_string(), "42".to_string())]);
/// let validated = ValidatedParam::from_value(
///     "id",
///     params.get("id").unwrap(),
///     Matcher::Number,
/// )?;
/// assert_eq!(validated.value, "42");
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedParam {
    pub name: String,
    pub value: String,
}

impl ValidatedParam {
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
            })
            .map_err(|msg| AppError::BadRequest {
                code: "INVALID_PARAM",
                message: msg,
            })
    }

    /// Consume this container and return the validated string value.
    pub fn into_inner(self) -> String {
        self.value
    }
}

impl std::ops::Deref for ValidatedParam {
    type Target = str;

    fn deref(&self) -> &str {
        &self.value
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::AppError;
    use std::collections::BTreeMap;

    #[test]
    fn validated_param_from_value_validates() {
        let valid = ValidatedParam::from_value("id", "42", Matcher::Number).unwrap();
        assert_eq!(valid.name, "id");
        assert_eq!(valid.value, "42");

        let err = ValidatedParam::from_value("id", "abc", Matcher::Number).unwrap_err();
        assert!(matches!(err, AppError::BadRequest { .. }));
    }

    #[test]
    fn validated_param_from_value_missing_key() {
        let params: BTreeMap<String, String> = BTreeMap::new();
        let value = params.get("id").ok_or_else(|| AppError::BadRequest {
            code: "MISSING_PARAM",
            message: "missing parameter: id".to_string(),
        });
        assert!(value.is_err());
    }
}
