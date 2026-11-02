//! Secret detection and value redaction.
//!
//! Provides heuristics for detecting secret-valued environment keys and
//! redacting sensitive values for display, so secrets never leak into CLI
//! output, logs, or error responses.

/// Heuristic: is this key likely to hold a secret?
///
/// Covers common secret suffixes/prefixes (`SECRET`, `PASSWORD`, `TOKEN`,
/// `_KEY`, `_PASS`, `_PWD`, `_CRED`, `_PRIVATE_KEY`, `STRIPE_*`, etc.).
///
/// Why: The original heuristic only matched `SECRET`/`PASSWORD`/`TOKEN`/`_KEY`,
/// so keys like `APP_PASS`, `STRIPE_SECRET_KEY`, or `DB_PWD` would print
/// plaintext. We err on the side of redacting too much rather than too little.
///
/// Revisit if: the heuristic proves too broad or too narrow for real-world
/// key names.
pub fn is_secret_key(key: &str) -> bool {
    let upper = key.to_ascii_uppercase();

    // Broad substring matches (covers keys with the word anywhere).
    if upper.contains("SECRET")
        || upper.contains("PASSWORD")
        || upper.contains("TOKEN")
        || upper.contains("_PASS")
        || upper.contains("_PWD")
        || upper.contains("_CRED")
        || upper.contains("_KEY")
    {
        return true;
    }

    // Exact matches for standalone names.
    upper == "KEY"
        || upper == "SECRET"
        || upper == "PASSWORD"
        || upper == "PASS"
        || upper == "PWD"
        || upper == "TOKEN"
        || upper.starts_with("STRIPE_")
        || upper.starts_with("PRIVATE_KEY")
}

/// Heuristic: does this value look like a URL containing credentials?
pub fn value_looks_sensitive(value: &str) -> bool {
    if let Some(rest) = value.split("://").nth(1)
        && let Some(authority) = rest.split('/').next()
    {
        return authority.contains('@') && authority.split('@').next().unwrap_or("").contains(':');
    }
    false
}

/// Redact a value for display when either the key or the value looks
/// sensitive.
///
/// Uses both key-based detection ([`is_secret_key`]) and value-based
/// detection ([`value_looks_sensitive`]), so a value like
/// `postgres://admin:password@localhost/db` is redacted even when the key
/// does not match the secret heuristics.
pub fn redact_value(key: &str, value: &str) -> String {
    if is_secret_key(key) || value_looks_sensitive(value) {
        "[redacted]".to_string()
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_secret_keys() {
        assert_eq!(redact_value("APP_KEY", "secret"), "[redacted]");
        assert_eq!(redact_value("PASSWORD", "x"), "[redacted]");
        assert_eq!(redact_value("API_TOKEN", "x"), "[redacted]");
        assert_eq!(redact_value("HOST", "localhost"), "localhost");
    }

    #[test]
    fn redacts_credential_urls() {
        assert_eq!(
            redact_value("DATABASE_URL", "postgres://user:pass@host/db"),
            "[redacted]"
        );
        assert_eq!(
            redact_value("DATABASE_URL", "postgres://host/db"),
            "postgres://host/db"
        );
    }

    #[test]
    fn detects_secret_keys() {
        assert!(is_secret_key("APP_KEY"));
        assert!(is_secret_key("API_TOKEN"));
        assert!(is_secret_key("DB_PASSWORD"));
        assert!(!is_secret_key("HOST"));
        assert!(!is_secret_key("PORT"));
    }

    #[test]
    fn redact_value_redacts_secret_keys() {
        // S-m1: broadened detection must catch keys the old heuristic missed.
        // These must be redacted, not printed as plaintext.
        assert_eq!(redact_value("APP_PASS", "s3cret"), "[redacted]");
        assert_eq!(redact_value("DB_PWD", "s3cret"), "[redacted]");
        assert_eq!(
            redact_value("STRIPE_SECRET_KEY", "sk_live_xxx"),
            "[redacted]"
        );
        assert_eq!(redact_value("APP_CRED", "s3cret"), "[redacted]");
        assert_eq!(redact_value("MY_SECRET_VALUE", "s3cret"), "[redacted]");
        assert_eq!(redact_value("ACCESS_TOKEN", "tok123"), "[redacted]");
        // Non-secret keys must not be redacted.
        assert_eq!(redact_value("HOST", "localhost"), "localhost");
        assert_eq!(redact_value("PORT", "3000"), "3000");
    }

    #[test]
    fn redact_value_redacts_credential_urls_regardless_of_key() {
        // A value with user:pass@host is redacted even when the key is not a
        // secret key.
        assert_eq!(
            redact_value("DATABASE_URL", "postgres://admin:password@localhost/db"),
            "[redacted]"
        );
        assert_eq!(
            redact_value("DATABASE_URL", "postgres://localhost/db"),
            "postgres://localhost/db"
        );
        assert_eq!(
            redact_value("NOT_A_SECRET_KEY", "postgres://admin:password@localhost/db"),
            "[redacted]"
        );
    }
}
