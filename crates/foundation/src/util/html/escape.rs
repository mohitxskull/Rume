use std::borrow::Cow;

/// Escapes HTML special characters (`&`, `<`, `>`, `"`, `'`) for safe embedding
/// in HTML text nodes and double-quoted attribute values.
///
/// This is **not** a general-purpose encoder for unquoted HTML attributes, `<script>` /
/// JavaScript blocks, CSS, or `javascript:` URLs; those contexts require context-specific
/// encoding or validation.
///
/// Features a fast-path scan: if no special characters are found (the common case
/// for clean inputs), it returns `Cow::Borrowed` with zero allocations.
///
/// # Examples
///
/// ```
/// use rume_foundation::util::html::escape_html;
///
/// assert_eq!(escape_html("Hello World"), "Hello World");
/// assert_eq!(
///     escape_html("<script>alert('xss')</script>"),
///     "&lt;script&gt;alert(&#x27;xss&#x27;)&lt;/script&gt;"
/// );
/// ```
pub fn escape_html(input: &str) -> Cow<'_, str> {
    let needs_escape = input
        .bytes()
        .any(|b| matches!(b, b'&' | b'<' | b'>' | b'"' | b'\''));

    if !needs_escape {
        return Cow::Borrowed(input);
    }

    let mut out = String::with_capacity(input.len() + 16);
    for ch in input.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#x27;"),
            _ => out.push(ch),
        }
    }
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_html_clean_input_borrows() {
        let input = "clean input 123";
        let res = escape_html(input);
        assert!(matches!(res, Cow::Borrowed(_)));
        assert_eq!(res, input);
    }

    #[test]
    fn escape_html_escapes_entities() {
        assert_eq!(
            escape_html("a & b < c > d \"e\" 'f'"),
            "a &amp; b &lt; c &gt; d &quot;e&quot; &#x27;f&#x27;"
        );
    }
}
