//! Case conversion helpers.
//!
//! Converts between camelCase, PascalCase, snake_case, and kebab-case.
//! Simple and ASCII-only.

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
