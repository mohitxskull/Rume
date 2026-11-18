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
    pub(crate) fn from_figment_with_secrets(err: &figment::Error, known_secrets: &[&str]) -> Self {
        let redacted = crate::env::secrets::redact_diagnostic_text(&err.to_string(), known_secrets);
        Self::Validation(redacted)
    }

    #[allow(dead_code)]
    pub(crate) fn from_figment(err: figment::Error) -> Self {
        Self::from_figment_with_secrets(&err, &[])
    }
}

impl From<figment::Error> for EnvError {
    fn from(err: figment::Error) -> Self {
        let redacted = crate::env::secrets::redact_diagnostic_text(&err.to_string(), &[]);
        Self::Validation(redacted)
    }
}
