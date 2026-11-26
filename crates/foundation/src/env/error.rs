//! Environment error types.

use std::path::PathBuf;
use thiserror::Error;

/// Errors that can occur while loading and resolving the environment.
#[derive(Debug, Error)]
pub enum EnvError {
    #[error("I/O error while reading {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to parse environment file {path} at line {line}: {message}")]
    Dotenv {
        path: PathBuf,
        line: usize,
        message: String,
    },

    #[error("failed to interpolate ${{{variable}}}: variable not found")]
    Interpolation { variable: String },

    #[error("circular reference detected while interpolating ${{{variable}}}")]
    CircularReference { variable: String },

    #[error("maximum interpolation depth ({depth}) exceeded while interpolating ${{{variable}}}")]
    MaxDepthExceeded { variable: String, depth: usize },

    #[error("interpolation budget exceeded while interpolating ${{{variable}}}: {reason}")]
    BudgetExceeded { variable: String, reason: String },

    #[error("failed to validate environment: {0}")]
    Validation(String),
}

impl EnvError {
    pub(crate) fn from_figment_with_secrets(
        err: figment::Error,
        known_secrets: &[&str],
        secret_keys: &std::collections::BTreeSet<String>,
    ) -> Self {
        Self::Validation(redact_figment_error(err, known_secrets, secret_keys))
    }
}

/// Structurally redact a `figment::Error` (handling both typed `Kind` payloads
/// and rendered diagnostic text) so secrets cannot leak even when they contain
/// escaped quotes or appear in non-string figment error variants.
pub(crate) fn redact_figment_error(
    err: figment::Error,
    known_secrets: &[&str],
    secret_keys: &std::collections::BTreeSet<String>,
) -> String {
    let messages: Vec<String> = err
        .into_iter()
        .map(|mut sub_err| {
            let is_secret_field = sub_err.path.iter().any(|segment| {
                crate::env::secrets::is_secret_key(segment)
                    || secret_keys
                        .iter()
                        .any(|key| key.eq_ignore_ascii_case(segment))
            });
            redact_figment_kind(&mut sub_err.kind, is_secret_field, known_secrets);
            crate::env::secrets::redact_diagnostic_text(&sub_err.to_string(), known_secrets)
        })
        .collect();
    messages.join("\n")
}

impl From<figment::Error> for EnvError {
    fn from(err: figment::Error) -> Self {
        Self::from_figment_with_secrets(err, &[], &std::collections::BTreeSet::new())
    }
}

fn redact_figment_kind(
    kind: &mut figment::error::Kind,
    is_secret_field: bool,
    known_secrets: &[&str],
) {
    use figment::error::{Actual, Kind};

    let contains_known_secret = |text: &str| {
        known_secrets
            .iter()
            .any(|secret| !secret.trim().is_empty() && text.contains(secret))
    };

    match kind {
        Kind::InvalidType(actual, _)
        | Kind::InvalidValue(actual, _)
        | Kind::Unsupported(actual)
        | Kind::UnsupportedKey(actual, _) => {
            if is_secret_field {
                *actual = Actual::Other("[redacted]".to_string());
            } else if let Actual::Str(text) = actual {
                if contains_known_secret(text) {
                    *actual = Actual::Other("[redacted]".to_string());
                } else if crate::env::secrets::value_looks_sensitive(text) {
                    *text = crate::env::secrets::redact_credential_urls_in_text(text);
                }
            }
        }
        Kind::UnknownVariant(variant, _) => {
            if is_secret_field
                || contains_known_secret(variant)
                || crate::env::secrets::value_looks_sensitive(variant)
            {
                *variant = "[redacted]".to_string();
            }
        }
        Kind::Message(msg) => {
            if is_secret_field {
                *msg = "[redacted]".to_string();
            } else {
                *msg = crate::env::secrets::redact_diagnostic_text(msg, known_secrets);
            }
        }
        Kind::InvalidLength(..)
        | Kind::UnknownField(..)
        | Kind::MissingField(..)
        | Kind::DuplicateField(..)
        | Kind::ISizeOutOfRange(..)
        | Kind::USizeOutOfRange(..) => {}
    }
}
