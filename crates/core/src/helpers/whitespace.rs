//! Whitespace condensation and normalization.

use std::borrow::Cow;

/// Collapses runs of whitespace into a single space and trims leading and trailing whitespace.
///
/// Returns `Cow::Borrowed` with zero allocations if the string is already clean.
///
/// # Examples
///
/// ```
/// use rume::helpers::condense_whitespace;
///
/// assert_eq!(condense_whitespace("hello   world"), "hello world");
/// assert_eq!(condense_whitespace("\n\t  hello \n world \t "), "hello world");
/// assert_eq!(condense_whitespace("clean input"), "clean input");
/// ```
pub fn condense_whitespace(input: &str) -> Cow<'_, str> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Cow::Borrowed("");
    }

    // Check if input is already condensed and trimmed.
    let needs_condense = input.len() != trimmed.len()
        || input.contains("  ")
        || input.chars().any(|ch| ch.is_whitespace() && ch != ' ');

    if !needs_condense {
        return Cow::Borrowed(input);
    }

    let mut out = String::with_capacity(trimmed.len());
    let mut prev_was_ws = false;

    for ch in trimmed.chars() {
        if ch.is_whitespace() {
            if !prev_was_ws {
                out.push(' ');
                prev_was_ws = true;
            }
        } else {
            out.push(ch);
            prev_was_ws = false;
        }
    }

    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn condense_clean_string_borrows() {
        let input = "clean string here";
        let res = condense_whitespace(input);
        assert!(matches!(res, Cow::Borrowed(_)));
        assert_eq!(res, input);
    }

    #[test]
    fn condense_removes_extra_spaces() {
        assert_eq!(condense_whitespace("  hello   world  "), "hello world");
        assert_eq!(
            condense_whitespace("one\n\n\ttwo\r\nthree"),
            "one two three"
        );
        assert_eq!(condense_whitespace("   "), "");
    }
}
