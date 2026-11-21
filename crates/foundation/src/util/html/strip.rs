use std::borrow::Cow;

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
/// use rume_foundation::util::html::strip_html_tags;
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
    // Why: once a forward scan for `-->` reaches EOF without finding one, no later
    // `<!--` opener in the remaining suffix can have a terminator either; caching
    // `has_comment_end = false` prevents O(N²) rescans on repeated unterminated `<!--`.
    let mut has_comment_end = true;
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        if ch == '<' {
            // Why: HTML/XML comments (`<!-- ... -->`) terminate at `-->` and do not
            // obey attribute quoting rules (an apostrophe like `<!-- don't show -->`
            // must not open a quote state and swallow the rest of the input).
            if has_comment_end && chars.get(i..i + 4) == Some(&['<', '!', '-', '-']) {
                let mut j = i + 4;
                let mut found_comment_end = false;
                while j + 2 < chars.len() {
                    if chars[j] == '-' && chars[j + 1] == '-' && chars[j + 2] == '>' {
                        found_comment_end = true;
                        j += 3;
                        break;
                    }
                    j += 1;
                }
                if found_comment_end {
                    pending_separator = true;
                    i = j;
                    continue;
                }
                has_comment_end = false;
            }

            // Why: an HTML/XML tag starts with `<` followed by a valid XML name-start
            // character (Unicode alphabetic, `_`, `:`) or `/`, `!`, `?`, and ends at the
            // first unquoted `>` before any nested unquoted `<`. Scanning ahead with these
            // rules prevents premature termination on `>` inside quoted attributes (e.g.
            // `<a title="a > b">`) and preserves literal `<` in prose (e.g.
            // `"Price < 100 USD"` or `"<p>Price < 100 USD</p>"`).
            let looks_like_tag = chars
                .get(i + 1)
                .is_some_and(|c| c.is_alphabetic() || matches!(c, '_' | ':' | '/' | '!' | '?'));
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn strip_html_tags_handles_quoted_gt_and_unclosed_lt() {
        assert_eq!(strip_html_tags(r#"<a title="a > b">Click</a>"#), "Click");
        assert_eq!(strip_html_tags("<a title='x > y'>Link</a>"), "Link");
        assert_eq!(strip_html_tags("Price < 100 USD"), "Price < 100 USD");
        assert_eq!(strip_html_tags("<p>Price < 100 USD</p>"), "Price < 100 USD");
        assert_eq!(strip_html_tags("<_item>x</_item>"), "x");
        assert_eq!(strip_html_tags("<é>x</é>"), "x");
        assert_eq!(strip_html_tags("<!-- don't show -->Visible"), "Visible");

        // Repeated unterminated comment openers must complete in linear time.
        let repeated = "<!--x".repeat(20_000);
        assert_eq!(strip_html_tags(&repeated), repeated);
    }
}
