//! Case conversion helpers.
//!
//! Converts between camelCase, PascalCase, snake_case, kebab-case,
//! Title Case, dot.case, and Sentence case.
//! Simple and ASCII-only.

/// Convert camelCase / PascalCase / mixed strings to snake_case.
///
/// `"userName"` → `"user_name"`, `"HTTPServer"` → `"h_t_t_p_server"`.
///
/// Every uppercase letter starts a new word (with an `_` prefix unless it is
/// the first character or already follows a separator) and is lowercased.
/// Existing `_`, `-`, `.`, and spaces are normalized to a single `_`.
pub fn snake_case(input: &str) -> String {
    let mut out = String::with_capacity(input.len() + 4);

    for (i, ch) in input.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if i != 0 && !out.is_empty() && !out.ends_with('_') {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else if ch == '_' || ch == '-' || ch == '.' || ch.is_ascii_whitespace() {
            if !out.is_empty() && !out.ends_with('_') {
                out.push('_');
            }
        } else {
            out.push(ch);
        }
    }

    if out.ends_with('_') {
        out.pop();
    }

    out
}

/// Convert snake_case / kebab-case / mixed strings to camelCase.
///
/// `"user_name"` → `"userName"`, `"_user"` → `"user"`.
///
/// Delimiters (`_`, `-`, `.`, spaces) are removed, and subsequent letters
/// are uppercased. Leading delimiters are stripped without capitalizing
/// the first letter.
pub fn camel_case(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut capitalize_next = false;

    for ch in input.chars() {
        if ch == '_' || ch == '-' || ch == '.' || ch.is_ascii_whitespace() {
            if !out.is_empty() {
                capitalize_next = true;
            }
        } else if out.is_empty() {
            out.push(ch.to_ascii_lowercase());
            capitalize_next = false;
        } else if capitalize_next {
            out.push(ch.to_ascii_uppercase());
            capitalize_next = false;
        } else {
            out.push(ch);
        }
    }

    out
}

/// Convert snake_case / kebab-case / mixed strings to PascalCase.
///
/// `"user_name"` → `"UserName"`, `"user"` → `"User"`.
///
/// Like [`camel_case`] but the first letter is always uppercased.
pub fn pascal_case(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut capitalize_next = true;

    for ch in input.chars() {
        if ch == '_' || ch == '-' || ch == '.' || ch.is_ascii_whitespace() {
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

/// Convert camelCase / PascalCase / mixed strings to kebab-case.
///
/// `"userName"` → `"user-name"`, `"HTTPServer"` → `"h-t-t-p-server"`.
///
/// Like [`snake_case`] but with `-` separators instead of `_`.
pub fn kebab_case(input: &str) -> String {
    let mut out = String::with_capacity(input.len() + 4);

    for (i, ch) in input.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if i != 0 && !out.is_empty() && !out.ends_with('-') {
                out.push('-');
            }
            out.push(ch.to_ascii_lowercase());
        } else if ch == '_' || ch == '-' || ch == '.' || ch.is_ascii_whitespace() {
            if !out.is_empty() && !out.ends_with('-') {
                out.push('-');
            }
        } else {
            out.push(ch);
        }
    }

    if out.ends_with('-') {
        out.pop();
    }

    out
}

/// Convert camelCase / snake_case / kebab-case to Title Case.
///
/// `"user_name"` → `"User Name"`, `"userProfile"` → `"User Profile"`.
///
/// Every word is separated by a space and starts with an uppercase letter.
pub fn title_case(input: &str) -> String {
    let mut out = String::with_capacity(input.len() + 4);
    let mut capitalize_next = true;

    for (i, ch) in input.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if i != 0 && !out.is_empty() && !out.ends_with(' ') {
                out.push(' ');
            }
            out.push(ch);
            capitalize_next = false;
        } else if ch == '_' || ch == '-' || ch == '.' || ch.is_ascii_whitespace() {
            if !out.is_empty() && !out.ends_with(' ') {
                out.push(' ');
            }
            capitalize_next = true;
        } else if capitalize_next {
            out.push(ch.to_ascii_uppercase());
            capitalize_next = false;
        } else {
            out.push(ch);
        }
    }

    if out.ends_with(' ') {
        out.pop();
    }

    out
}

/// Convert camelCase / snake_case / kebab-case to dot.case.
///
/// `"userName"` → `"user.name"`, `"user_profile"` → `"user.profile"`.
///
/// Useful for metric keys, event names, and nested configuration paths.
pub fn dot_case(input: &str) -> String {
    let mut out = String::with_capacity(input.len() + 4);

    for (i, ch) in input.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if i != 0 && !out.is_empty() && !out.ends_with('.') {
                out.push('.');
            }
            out.push(ch.to_ascii_lowercase());
        } else if ch == '_' || ch == '-' || ch == '.' || ch.is_ascii_whitespace() {
            if !out.is_empty() && !out.ends_with('.') {
                out.push('.');
            }
        } else {
            out.push(ch);
        }
    }

    if out.ends_with('.') {
        out.pop();
    }

    out
}

/// Convert camelCase / snake_case / kebab-case to Sentence case.
///
/// `"user_name"` → `"User name"`, `"userProfile"` → `"User profile"`.
///
/// The first character is capitalized, and all subsequent words are lowercased
/// separated by a space.
pub fn sentence_case(input: &str) -> String {
    let mut out = String::with_capacity(input.len() + 4);

    for (i, ch) in input.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if i != 0 && !out.is_empty() && !out.ends_with(' ') {
                out.push(' ');
            }
            out.push(ch.to_ascii_lowercase());
        } else if ch == '_' || ch == '-' || ch == '.' || ch.is_ascii_whitespace() {
            if !out.is_empty() && !out.ends_with(' ') {
                out.push(' ');
            }
        } else {
            out.push(ch.to_ascii_lowercase());
        }
    }

    if out.ends_with(' ') {
        out.pop();
    }

    if let Some(first) = out.chars().next()
        && first.is_ascii_lowercase()
    {
        out.replace_range(..1, &first.to_ascii_uppercase().to_string());
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
        assert_eq!(snake_case("user_Name"), "user_name");
        assert_eq!(snake_case("_user_name_"), "user_name");
    }

    #[test]
    fn camel_case_converts_snake_case() {
        assert_eq!(camel_case("user_name"), "userName");
        assert_eq!(camel_case("http_server"), "httpServer");
        assert_eq!(camel_case("_user"), "user");
        assert_eq!(camel_case("__user_name__"), "userName");
    }

    #[test]
    fn pascal_case_converts_snake_case() {
        assert_eq!(pascal_case("user_name"), "UserName");
        assert_eq!(pascal_case("http_server"), "HttpServer");
        assert_eq!(pascal_case("_user"), "User");
    }

    #[test]
    fn kebab_case_converts_camel_case() {
        assert_eq!(kebab_case("userName"), "user-name");
        assert_eq!(kebab_case("HTTPServer"), "h-t-t-p-server");
        assert_eq!(kebab_case("-user-name-"), "user-name");
    }

    #[test]
    fn title_case_converts_various_inputs() {
        assert_eq!(title_case("user_name"), "User Name");
        assert_eq!(title_case("userName"), "User Name");
        assert_eq!(title_case("user-profile"), "User Profile");
        assert_eq!(title_case("already Title Case"), "Already Title Case");
    }

    #[test]
    fn dot_case_converts_various_inputs() {
        assert_eq!(dot_case("userName"), "user.name");
        assert_eq!(dot_case("user_profile"), "user.profile");
        assert_eq!(dot_case("user-role"), "user.role");
    }

    #[test]
    fn sentence_case_converts_various_inputs() {
        assert_eq!(sentence_case("userName"), "User name");
        assert_eq!(sentence_case("user_profile"), "User profile");
        assert_eq!(sentence_case("user-status-code"), "User status code");
    }
}
