//! Text truncation with UTF-8 safety and zero-allocation fast paths.

use std::borrow::Cow;

/// Options for string truncation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TruncateOptions<'a> {
    /// Suffix appended to truncated strings. Defaults to `"..."`.
    pub suffix: &'a str,
    /// When true, prevents chopping in the middle of a word by rolling back
    /// to the previous whitespace boundary.
    pub complete_words: bool,
}

impl Default for TruncateOptions<'_> {
    fn default() -> Self {
        Self {
            suffix: "...",
            complete_words: false,
        }
    }
}

/// Truncate a string to `limit` characters using default options (`"..."` suffix).
///
/// Returns a borrowed slice with zero allocations when `input` does not exceed `limit`.
/// Operates strictly on Unicode scalar boundaries, ensuring it never panics on multi-byte characters.
///
/// # Examples
///
/// ```
/// use rume::helpers::truncate;
///
/// assert_eq!(truncate("hello world", 20), "hello world");
/// assert_eq!(truncate("hello world", 8), "hello...");
/// assert_eq!(truncate("🦀🦀🦀🦀🦀", 4), "🦀...");
/// ```
pub fn truncate(input: &str, limit: usize) -> Cow<'_, str> {
    truncate_with(input, limit, TruncateOptions::default())
}

/// Truncate a string to `limit` characters with custom options.
///
/// # Examples
///
/// ```
/// use rume::helpers::{truncate_with, TruncateOptions};
///
/// let opts = TruncateOptions {
///     suffix: " [more]",
///     complete_words: true,
/// };
/// assert_eq!(
///     truncate_with("this is a long sentence", 16, opts),
///     "this is a [more]"
/// );
/// ```
pub fn truncate_with<'a>(
    input: &'a str,
    limit: usize,
    options: TruncateOptions<'_>,
) -> Cow<'a, str> {
    if limit == 0 {
        return Cow::Borrowed("");
    }

    let char_count = input.chars().count();
    if char_count <= limit {
        return Cow::Borrowed(input);
    }

    let suffix_len = options.suffix.chars().count();
    if limit <= suffix_len {
        let byte_end = input
            .char_indices()
            .nth(limit)
            .map(|(idx, _)| idx)
            .unwrap_or(input.len());
        return Cow::Borrowed(&input[..byte_end]);
    }

    let prefix_char_limit = limit - suffix_len;
    let byte_limit = input
        .char_indices()
        .nth(prefix_char_limit)
        .map(|(idx, _)| idx)
        .unwrap_or(input.len());

    let candidate = &input[..byte_limit];

    let final_prefix = if options.complete_words {
        let next_char_is_ws = input[byte_limit..]
            .chars()
            .next()
            .map(|c| c.is_whitespace())
            .unwrap_or(true);

        if next_char_is_ws {
            candidate.trim_end()
        } else if let Some(last_ws) = candidate.rfind(char::is_whitespace) {
            let trimmed = candidate[..last_ws].trim_end();
            if !trimmed.is_empty() {
                trimmed
            } else {
                candidate
            }
        } else {
            candidate
        }
    } else {
        candidate
    };

    let trimmed_prefix = final_prefix.trim_end();
    let mut out = String::with_capacity(trimmed_prefix.len() + options.suffix.len());
    out.push_str(trimmed_prefix);
    out.push_str(options.suffix);
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_under_limit_borrows() {
        let input = "hello";
        let res = truncate(input, 10);
        assert!(matches!(res, Cow::Borrowed(_)));
        assert_eq!(res, "hello");
    }

    #[test]
    fn truncate_at_limit_borrows() {
        let input = "hello";
        let res = truncate(input, 5);
        assert!(matches!(res, Cow::Borrowed(_)));
        assert_eq!(res, "hello");
    }

    #[test]
    fn truncate_over_limit_truncates_with_suffix() {
        let input = "this is a long title";
        assert_eq!(truncate(input, 10), "this is...");
    }

    #[test]
    fn truncate_complete_words() {
        let input = "this is a very long title";
        let opts = TruncateOptions {
            suffix: "...",
            complete_words: true,
        };
        assert_eq!(truncate_with(input, 12, opts), "this is a...");

        // Slicing mid-word rolls back to previous complete word
        assert_eq!(truncate_with(input, 13, opts), "this is a...");
    }

    #[test]
    fn truncate_unicode_safe() {
        let input = "🦀🦀🦀🦀🦀";
        assert_eq!(truncate(input, 4), "🦀...");
        let accented = "café crème brûlée";
        assert_eq!(truncate(accented, 8), "café...");
    }

    #[test]
    fn truncate_edge_cases() {
        assert_eq!(truncate("", 5), "");
        assert_eq!(truncate("hello", 0), "");
        assert_eq!(truncate("hello", 2), "he");
    }
}
