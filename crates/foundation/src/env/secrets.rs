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
        || upper.contains("APIKEY")
        || upper.contains("AUTHORIZATION")
        || upper.contains("BEARER")
        || upper.contains("SESSION_ID")
        || upper.contains("SESSION")
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
        || upper == "APIKEY"
        || upper == "AUTHORIZATION"
        || upper == "BEARER"
        || upper == "SESSION"
        || upper == "SESSION_ID"
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

/// Redact passwords from credential URLs found inside arbitrary text.
pub fn redact_credential_urls_in_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;

    while let Some(proto_pos) = rest.find("://") {
        let before = &rest[..proto_pos + 3];
        out.push_str(before);
        rest = &rest[proto_pos + 3..];

        // Find the boundary of the authority/URL
        let end_authority = rest
            .find([
                '/', ' ', '"', '\'', '\n', '\r', '\t', '>', '<', ')', ']', '}',
            ])
            .unwrap_or(rest.len());
        let authority = &rest[..end_authority];

        if let Some(at_pos) = authority.rfind('@') {
            let user_info = &authority[..at_pos];
            if let Some(colon_pos) = user_info.find(':') {
                let user = &user_info[..colon_pos];
                let host_and_rest = &authority[at_pos..];
                out.push_str(user);
                out.push_str(":[redacted]");
                out.push_str(host_and_rest);
                rest = &rest[end_authority..];
                continue;
            }
        }

        out.push_str(authority);
        rest = &rest[end_authority..];
    }

    out.push_str(rest);
    out
}

fn line_targets_secret_field(line: &str) -> bool {
    // 1. Check for `key "..."` in Figment error
    if let Some(pos) = line.find("key \"") {
        let start = pos + 5;
        if let Some(end) = line[start..].find('"') {
            let key_path = &line[start..start + end];
            if key_path.split('.').any(is_secret_key) {
                return true;
            }
        }
    }

    // 2. Check for `<field>: ...` at start of line
    if let Some(pos) = line.find(':') {
        let candidate = line[..pos].trim();
        if !candidate.is_empty()
            && !candidate.contains(' ')
            && candidate.split('.').any(is_secret_key)
        {
            return true;
        }
    }

    false
}

fn redact_secret_field_lines(text: &str) -> String {
    let mut out_lines = Vec::new();
    for line in text.lines() {
        if line_targets_secret_field(line) {
            out_lines.push(redact_line_values(line));
        } else {
            out_lines.push(line.to_string());
        }
    }
    out_lines.join("\n")
}

fn replace_quoted_after(mut text: String, prefix: &str) -> String {
    let mut search_from = 0;
    while let Some(pos) = text[search_from..].find(prefix) {
        let start = search_from + pos + prefix.len();
        if let Some(end) = text[start..].find('"') {
            text.replace_range(start..start + end, "[redacted]");
            search_from = start + "[redacted]".len();
        } else {
            break;
        }
    }
    text
}

fn redact_line_values(line: &str) -> String {
    let mut out = line.to_string();
    out = replace_quoted_after(out, "string \"");
    out = replace_quoted_after(out, "invalid value: \"");
    out = replace_quoted_after(out, "found \"");
    out
}

/// Redact sensitive plaintext, secret key values, and credential URLs from
/// diagnostics, logs, and error messages.
pub fn redact_diagnostic_text(text: &str, known_secrets: &[&str]) -> String {
    let mut result = text.to_string();

    // 1. Redact all explicitly known secret values
    for secret in known_secrets {
        if !secret.trim().is_empty() {
            result = result.replace(*secret, "[redacted]");
        }
    }

    // 2. Redact credential URLs in text
    result = redact_credential_urls_in_text(&result);

    // 3. Redact secret-field error lines
    result = redact_secret_field_lines(&result);

    result
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

    #[test]
    fn redact_diagnostic_text_redacts_known_secrets() {
        let msg = "failed to parse: token 'super_secret_xyz' is invalid";
        let redacted = redact_diagnostic_text(msg, &["super_secret_xyz"]);
        assert_eq!(redacted, "failed to parse: token '[redacted]' is invalid");
    }

    #[test]
    fn redact_diagnostic_text_redacts_credential_urls() {
        let msg = "error: cannot connect to postgres://admin:super_secret_pw@localhost:5432/mydb";
        let redacted = redact_diagnostic_text(msg, &[]);
        assert_eq!(
            redacted,
            "error: cannot connect to postgres://admin:[redacted]@localhost:5432/mydb"
        );
    }

    #[test]
    fn redact_diagnostic_text_redacts_secret_key_error_lines() {
        let msg = "api_key: invalid type: string \"super_secret_key_123\", expected u32";
        let redacted = redact_diagnostic_text(msg, &[]);
        assert_eq!(
            redacted,
            "api_key: invalid type: string \"[redacted]\", expected u32"
        );
    }

    #[test]
    fn redact_diagnostic_text_preserves_non_secret_errors() {
        let msg = "port: invalid type: string \"not_a_number\", expected u16";
        let redacted = redact_diagnostic_text(msg, &[]);
        assert_eq!(redacted, msg);
    }
}
