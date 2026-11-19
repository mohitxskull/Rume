//! HTML entity escaping, tag stripping, and rich-text excerpt generation.

use std::borrow::Cow;

use super::truncate::truncate;
use super::whitespace::condense_whitespace;

/// Escapes HTML special characters (`&`, `<`, `>`, `"`, `'`) for safe embedding.
///
/// Features a fast-path scan: if no special characters are found (the common case
/// for clean inputs), it returns `Cow::Borrowed` with zero allocations.
///
/// # Examples
///
/// ```
/// use rume_foundation::helpers::escape_html;
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

/// Strips all HTML/XML tags from a string and decodes standard entities.
///
/// Ensures block tags (e.g. `</h1><p>`) or inline tags abutting words are separated
/// by whitespace so words do not inadvertently concatenate together, while avoiding
/// synthetic whitespace before punctuation following closing tags (e.g. `<strong>World</strong>!`).
///
/// Returns `Cow::Borrowed` if the string contains neither `<` nor `&`.
///
/// # Examples
///
/// ```
/// use rume_foundation::helpers::strip_html_tags;
///
/// assert_eq!(
///     strip_html_tags("<p>Hello <strong>World</strong>!</p>"),
///     "Hello World!"
/// );
/// ```
pub fn strip_html_tags(input: &str) -> Cow<'_, str> {
    if !input.contains('<') && !input.contains('&') {
        return Cow::Borrowed(input);
    }

    let mut out = String::with_capacity(input.len());
    let mut tag_name = String::new();
    let mut pending_separator = false;

    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        if ch == '<' {
            // Why: an HTML/XML tag starts with `<` followed by an ASCII letter,
            // `/`, `!`, or `?`, and ends at the first unquoted `>` before any
            // nested unquoted `<`. Scanning ahead with these rules prevents
            // premature termination on `>` inside quoted attributes (e.g.
            // `<a title="a > b">`) and preserves literal `<` in prose (e.g.
            // `"Price < 100 USD"` or `"<p>Price < 100 USD</p>"`).
            let looks_like_tag = chars
                .get(i + 1)
                .is_some_and(|c| c.is_ascii_alphabetic() || matches!(c, '/' | '!' | '?'));
            let mut j = i + 1;
            let mut quote: Option<char> = None;
            let mut found_close = false;
            if looks_like_tag {
                while j < chars.len() {
                    let c = chars[j];
                    if let Some(q) = quote {
                        if c == q {
                            quote = None;
                        }
                    } else if c == '"' || c == '\'' {
                        quote = Some(c);
                    } else if c == '<' {
                        break;
                    } else if c == '>' {
                        found_close = true;
                        break;
                    }
                    j += 1;
                }
            }

            if found_close {
                let mut name_idx = i + 1;
                if name_idx < j && chars[name_idx] == '/' {
                    name_idx += 1;
                }
                tag_name.clear();
                while name_idx < j && (chars[name_idx].is_alphanumeric() || chars[name_idx] == ':')
                {
                    tag_name.push(chars[name_idx].to_ascii_lowercase());
                    name_idx += 1;
                }
                let is_block_tag = matches!(
                    tag_name.as_str(),
                    "p" | "div"
                        | "h1"
                        | "h2"
                        | "h3"
                        | "h4"
                        | "h5"
                        | "h6"
                        | "br"
                        | "hr"
                        | "li"
                        | "ul"
                        | "ol"
                        | "tr"
                        | "td"
                        | "th"
                        | "blockquote"
                        | "pre"
                        | "article"
                        | "section"
                        | "header"
                        | "footer"
                        | "nav"
                );
                if is_block_tag {
                    if !out.is_empty() && !out.ends_with(' ') {
                        out.push(' ');
                    }
                    pending_separator = false;
                } else {
                    pending_separator = true;
                }
                i = j + 1;
                continue;
            }
        }

        if ch == '&' {
            let mut entity = String::new();
            let mut j = i + 1;
            while j < chars.len()
                && j - i < 10
                && chars[j] != ';'
                && !chars[j].is_whitespace()
                && chars[j] != '<'
            {
                entity.push(chars[j]);
                j += 1;
            }
            if j < chars.len() && chars[j] == ';' {
                let decoded_char = match entity.as_str() {
                    "amp" => Some('&'),
                    "lt" => Some('<'),
                    "gt" => Some('>'),
                    "quot" => Some('"'),
                    "#39" | "#x27" | "apos" => Some('\''),
                    "nbsp" => Some(' '),
                    _ => None,
                };
                if let Some(dec) = decoded_char {
                    if pending_separator {
                        let prev_is_alnum = out.chars().last().is_some_and(|c| c.is_alphanumeric());
                        if prev_is_alnum && dec.is_alphanumeric() && !out.ends_with(' ') {
                            out.push(' ');
                        }
                        pending_separator = false;
                    }
                    out.push(dec);
                    i = j + 1;
                    continue;
                }
            }
        }

        if pending_separator {
            let prev_is_alnum = out.chars().last().is_some_and(|c| c.is_alphanumeric());
            if prev_is_alnum && ch.is_alphanumeric() && !out.ends_with(' ') {
                out.push(' ');
            }
            pending_separator = false;
        }

        out.push(ch);
        i += 1;
    }

    if out.ends_with(' ') {
        out.pop();
    }

    Cow::Owned(out)
}

/// Extracts a plain-text excerpt from rich text or HTML by stripping tags,
/// condensing whitespace, and truncating to `limit` characters.
///
/// # Examples
///
/// ```
/// use rume_foundation::helpers::excerpt;
///
/// let html = "<p>Rume is an <strong>explicit</strong> application shell for Axum.</p>";
/// assert_eq!(excerpt(html, 25), "Rume is an explicit ap...");
/// ```
pub fn excerpt(input: &str, limit: usize) -> Cow<'_, str> {
    let stripped = strip_html_tags(input);
    let condensed = condense_whitespace(&stripped);
    match truncate(&condensed, limit) {
        Cow::Borrowed(_)
            if matches!(stripped, Cow::Borrowed(_)) && matches!(condensed, Cow::Borrowed(_)) =>
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

    #[test]
    fn strip_html_tags_removes_tags() {
        assert_eq!(strip_html_tags("plain text"), "plain text");
        assert_eq!(
            strip_html_tags("<div><p>Hello <br/>World</p></div>").trim(),
            "Hello World"
        );
        assert_eq!(
            strip_html_tags("<p>Hello <strong>World</strong>!</p>"),
            "Hello World!"
        );
        assert_eq!(
            strip_html_tags("<span>Hello</span><span>World</span>"),
            "Hello World"
        );
        assert_eq!(strip_html_tags("Fish &amp; Chips"), "Fish & Chips");
    }

    #[test]
    fn excerpt_strips_and_truncates() {
        let html = "<div><h1>Hello World</h1><p>This is a paragraph.</p></div>";
        assert_eq!(excerpt(html, 15), "Hello World...");
    }

    #[test]
    fn strip_html_tags_handles_quoted_gt_and_unclosed_lt() {
        assert_eq!(strip_html_tags(r#"<a title="a > b">Click</a>"#), "Click");
        assert_eq!(strip_html_tags("<a title='x > y'>Link</a>"), "Link");
        assert_eq!(strip_html_tags("Price < 100 USD"), "Price < 100 USD");
        assert_eq!(strip_html_tags("<p>Price < 100 USD</p>"), "Price < 100 USD");
    }
}
