//! URL-friendly slug generation.

/// URL-friendly slug.
///
/// Lowercases the input, replaces every run of non-alphanumeric characters
/// with a single `-`, and trims leading and trailing dashes.
///
/// `"Hello ♥ World"` → `"hello-world"`.
pub fn slug(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut prev_was_dash = false;

    for ch in input.chars().map(|ch| ch.to_ascii_lowercase()) {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
            prev_was_dash = false;
        } else if !prev_was_dash && !out.is_empty() {
            out.push('-');
            prev_was_dash = true;
        }
    }

    out.trim_start_matches('-')
        .trim_end_matches('-')
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_produces_url_friendly_slug() {
        assert_eq!(slug("Hello World!"), "hello-world");
        assert_eq!(slug("Hello ♥ World"), "hello-world");
        assert_eq!(slug("---Multiple---dashes---"), "multiple-dashes");
    }
}
