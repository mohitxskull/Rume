//! Diagnostic redaction and sensitive data masking for logs and telemetry.

/// Masks an API key, token, or secret string, keeping leading and trailing characters
/// for operational debugging while redacting the sensitive payload.
///
/// UTF-8 safe; never panics on multi-byte characters.
///
/// # Examples
///
/// ```
/// use rume::helpers::mask_secret;
///
/// assert_eq!(mask_secret("sk_live_1234567890abcdef"), "sk_l...cdef");
/// assert_eq!(mask_secret("short"), "******");
/// ```
pub fn mask_secret(secret: &str) -> String {
    let char_count = secret.chars().count();
    if char_count <= 6 {
        return "******".to_string();
    }

    let edge_len = if char_count <= 12 { 2 } else { 4 };

    let prefix_end = secret
        .char_indices()
        .nth(edge_len)
        .map(|(i, _)| i)
        .unwrap_or(0);

    let suffix_start = secret
        .char_indices()
        .nth(char_count - edge_len)
        .map(|(i, _)| i)
        .unwrap_or(secret.len());

    format!("{}...{}", &secret[..prefix_end], &secret[suffix_start..])
}

/// Masks an email address for privacy-safe logs and error outputs.
///
/// If the input is not a valid email address (missing `@`), it falls back to `"******"`.
///
/// # Examples
///
/// ```
/// use rume::helpers::mask_email;
///
/// assert_eq!(mask_email("john.doe@example.com"), "j***e@example.com");
/// assert_eq!(mask_email("me@example.com"), "m***@example.com");
/// assert_eq!(mask_email("invalid-email"), "******");
/// ```
pub fn mask_email(email: &str) -> String {
    let Some((local, domain)) = email.split_once('@') else {
        return "******".to_string();
    };

    let local_count = local.chars().count();
    if local_count <= 2 {
        let first = local.chars().next().unwrap_or('*');
        format!("{first}***@{domain}")
    } else {
        let first = local.chars().next().unwrap();
        let last = local.chars().last().unwrap();
        format!("{first}***{last}@{domain}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mask_secret_lengths() {
        assert_eq!(mask_secret("12345"), "******");
        assert_eq!(mask_secret("123456"), "******");
        assert_eq!(mask_secret("1234567890"), "12...90");
        assert_eq!(mask_secret("sk_live_1234567890abcdef"), "sk_l...cdef");
    }

    #[test]
    fn mask_email_variants() {
        assert_eq!(mask_email("john.doe@example.com"), "j***e@example.com");
        assert_eq!(mask_email("a@b.com"), "a***@b.com");
        assert_eq!(mask_email("ab@example.com"), "a***@example.com");
        assert_eq!(mask_email("not-an-email"), "******");
    }
}
