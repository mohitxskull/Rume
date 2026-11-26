use std::borrow::Cow;

use super::strip_html_tags;
use crate::util::truncate::truncate;
use crate::util::whitespace::condense_whitespace;

/// Extracts a plain-text excerpt from rich text or HTML by stripping tags,
/// condensing whitespace, and truncating to `limit` characters.
///
/// # Examples
///
/// ```
/// use rume_foundation::util::html::excerpt;
///
/// let html = "<p>Rume is an <strong>explicit</strong> application shell for Axum.</p>";
/// assert_eq!(excerpt(html, 25), "Rume is an explicit ap...");
/// ```
pub fn excerpt(input: &str, limit: usize) -> Cow<'_, str> {
    let stripped = strip_html_tags(input);
    let condensed = condense_whitespace(&stripped);
    match truncate(&condensed, limit) {
        Cow::Borrowed(result)
            if matches!(stripped, Cow::Borrowed(_))
                && matches!(condensed, Cow::Borrowed(_))
                && result.len() == input.len() =>
        {
            Cow::Borrowed(input)
        }
        res => Cow::Owned(res.into_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn excerpt_strips_and_truncates() {
        let html = "<div><h1>Hello World</h1><p>This is a paragraph.</p></div>";
        assert_eq!(excerpt(html, 15), "Hello World...");
    }

    #[test]
    fn excerpt_strips_html_comments() {
        assert_eq!(excerpt("<!-- don't show -->Visible", 20), "Visible");
    }

    #[test]
    fn excerpt_whitespace_only_returns_empty() {
        assert_eq!(excerpt(" \t ", 20), "");
    }

    #[test]
    fn excerpt_short_borrowed_truncation_respects_limit() {
        assert_eq!(excerpt("hello", 2), "he");
    }
}
