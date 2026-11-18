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

    #[error("failed to parse environment file {path}: {source}")]
    Dotenv {
        path: PathBuf,
        #[source]
        source: dotenvy::Error,
    },

    #[error("failed to interpolate ${{{variable}}}: variable not found")]
    Interpolation { variable: String },

    #[error("circular reference detected while interpolating ${{{variable}}}")]
    CircularReference { variable: String },

    #[error("maximum interpolation depth ({depth}) exceeded while interpolating ${{{variable}}}")]
    MaxDepthExceeded { variable: String, depth: usize },

    #[error("failed to validate environment: {0}")]
    Validation(String),
}

impl EnvError {
    pub(crate) fn from_figment_with_secrets(
        err: figment::Error,
        known_secrets: &[&str],
        secret_keys: &std::collections::BTreeSet<String>,
    ) -> Self {
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
        Self::Validation(messages.join("\n"))
    }

    #[allow(dead_code)]
    pub(crate) fn from_figment(err: figment::Error) -> Self {
        Self::from_figment_with_secrets(err, &[], &std::collections::BTreeSet::new())
    }
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
