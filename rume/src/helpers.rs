//! Pure, hermetic string helpers.
//!
//! Inspired by AdonisJS `@adonisjs/helpers` (`safeEqual`, `slug`, case
//! conversions). Everything is implemented in `std` — no new dependencies,
//! no `unsafe`. Input is treated as ASCII; non-ASCII characters are passed
//! through unchanged except where lowercasing is explicitly applied.

/// Constant-time comparison of two strings.
///
/// Returns `true` only when `a` and `b` are identical. Lengths are compared
/// first (unequal lengths short-circuit to `false`), then every byte pair is
/// XOR-accumulated without any early exit, so the elapsed time does not
/// depend on how many bytes match or where they first differ.
///
/// Why: an early-exit byte comparison leaks the position of the first
/// differing byte through timing. Rejecting different lengths up front is
/// unavoidable (length itself is public), but beyond that no branch depends
/// on secret data.
///
/// Revisit if: a constant-time length comparison becomes necessary.
pub fn safe_equal(a: &str, b: &str) -> bool {
    let a_bytes = a.as_bytes();
    let b_bytes = b.as_bytes();

    if a_bytes.len() != b_bytes.len() {
        return false;
    }

    let mut diff = 0u8;
    for (x, y) in a_bytes.iter().zip(b_bytes.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

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

/// Convert camelCase / PascalCase to snake_case.
///
/// `"userName"` → `"user_name"`, `"HTTPResponse"` → `"h_t_t_p_response"`.
///
/// Simple and ASCII-only: every uppercase letter starts a new word (with an
/// `_` prefix unless it is the first character) and is lowercased. Existing
/// `_` and `-` separators are normalized to `_`.
pub fn snake_case(input: &str) -> String {
    let mut out = String::with_capacity(input.len() + 4);

    for (i, ch) in input.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if i != 0 {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else if ch == '_' || ch == '-' {
            if !out.ends_with('_') {
                out.push('_');
            }
        } else {
            out.push(ch);
        }
    }

    out
}

/// Convert snake_case / kebab-case to camelCase.
///
/// `"user_name"` → `"userName"`. The first character is left as-is; only
/// letters following `_` or `-` are uppercased.
pub fn camel_case(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut capitalize_next = false;

    for ch in input.chars() {
        if ch == '_' || ch == '-' {
            capitalize_next = true;
        } else if capitalize_next {
            out.push(ch.to_ascii_uppercase());
            capitalize_next = false;
        } else {
            out.push(ch);
        }
    }

    out
}

/// Convert snake_case / kebab-case to PascalCase.
///
/// `"user_name"` → `"UserName"`. Like [`camel_case`] but the first letter is
/// also uppercased.
pub fn pascal_case(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut capitalize_next = true;

    for ch in input.chars() {
        if ch == '_' || ch == '-' {
            capitalize_next = true;
        } else if capitalize_next {
            out.push(ch.to_ascii_uppercase());
            capitalize_next = false;
        } else {
            out.push(ch);
        }
    }

    out
}

/// Convert camelCase / PascalCase to kebab-case.
///
/// `"userName"` → `"user-name"`. Like [`snake_case`] but with `-`
/// separators instead of `_`.
pub fn kebab_case(input: &str) -> String {
    let mut out = String::with_capacity(input.len() + 4);

    for (i, ch) in input.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if i != 0 {
                out.push('-');
            }
            out.push(ch.to_ascii_lowercase());
        } else if ch == '_' || ch == '-' {
            if !out.ends_with('-') {
                out.push('-');
            }
        } else {
            out.push(ch);
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_equal_identical_returns_true() {
        assert!(safe_equal("", ""));
        assert!(safe_equal("abc", "abc"));
        assert!(safe_equal(
            "a very long secret value",
            "a very long secret value"
        ));
    }

    #[test]
    fn safe_equal_different_returns_false() {
        assert!(!safe_equal("abc", "abd"));
        assert!(!safe_equal("same prefix", "same suffix"));
    }

    #[test]
    fn safe_equal_different_length_returns_false() {
        assert!(!safe_equal("abc", "abcd"));
        assert!(!safe_equal("", "a"));
        assert!(!safe_equal("aaaa", "aaab"));
    }

    #[test]
    fn slug_produces_url_friendly_slug() {
        assert_eq!(slug("Hello World!"), "hello-world");
        assert_eq!(slug("Hello ♥ World"), "hello-world");
        assert_eq!(slug("---Multiple---dashes---"), "multiple-dashes");
    }

    #[test]
    fn snake_case_converts_camel_case() {
        assert_eq!(snake_case("userName"), "user_name");
        assert_eq!(snake_case("HTTPServer"), "h_t_t_p_server");
    }

    #[test]
    fn camel_case_converts_snake_case() {
        assert_eq!(camel_case("user_name"), "userName");
        assert_eq!(camel_case("http_server"), "httpServer");
    }

    #[test]
    fn pascal_case_converts_snake_case() {
        assert_eq!(pascal_case("user_name"), "UserName");
        assert_eq!(pascal_case("http_server"), "HttpServer");
    }

    #[test]
    fn kebab_case_converts_camel_case() {
        assert_eq!(kebab_case("userName"), "user-name");
        assert_eq!(kebab_case("HTTPServer"), "h-t-t-p-server");
    }
}
