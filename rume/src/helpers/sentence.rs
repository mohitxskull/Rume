//! Natural language list formatting.

/// Formats a list of items into a human-readable English sentence with default separators (`", "` and `" and "`).
///
/// Pre-computes exact byte capacity for a single heap allocation.
///
/// # Examples
///
/// ```
/// use rume::helpers::to_sentence;
///
/// assert_eq!(to_sentence(&["apple"]), "apple");
/// assert_eq!(to_sentence(&["apple", "banana"]), "apple and banana");
/// assert_eq!(to_sentence(&["apple", "banana", "orange"]), "apple, banana and orange");
/// ```
pub fn to_sentence<T: AsRef<str>>(items: &[T]) -> String {
    to_sentence_with(items, ", ", " and ")
}

/// Formats a list of items into a sentence using custom separators.
///
/// # Examples
///
/// ```
/// use rume::helpers::to_sentence_with;
///
/// let items = &["red", "green", "blue"];
/// assert_eq!(to_sentence_with(items, ", ", ", or "), "red, green, or blue");
/// ```
pub fn to_sentence_with<T: AsRef<str>>(
    items: &[T],
    separator: &str,
    last_separator: &str,
) -> String {
    match items.len() {
        0 => String::new(),
        1 => items[0].as_ref().to_string(),
        2 => {
            let mut out = String::with_capacity(
                items[0].as_ref().len() + last_separator.len() + items[1].as_ref().len(),
            );
            out.push_str(items[0].as_ref());
            out.push_str(last_separator);
            out.push_str(items[1].as_ref());
            out
        }
        len => {
            let total_item_len: usize = items.iter().map(|s| s.as_ref().len()).sum();
            let total_len = total_item_len + (separator.len() * (len - 2)) + last_separator.len();
            let mut out = String::with_capacity(total_len);

            for (i, item) in items.iter().enumerate() {
                out.push_str(item.as_ref());
                if i < len - 2 {
                    out.push_str(separator);
                } else if i == len - 2 {
                    out.push_str(last_separator);
                }
            }

            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_sentence_empty() {
        let empty: &[&str] = &[];
        assert_eq!(to_sentence(empty), "");
    }

    #[test]
    fn to_sentence_single() {
        assert_eq!(to_sentence(&["one"]), "one");
    }

    #[test]
    fn to_sentence_two_items() {
        assert_eq!(to_sentence(&["one", "two"]), "one and two");
    }

    #[test]
    fn to_sentence_three_or_more() {
        assert_eq!(to_sentence(&["one", "two", "three"]), "one, two and three");
        assert_eq!(to_sentence(&["a", "b", "c", "d"]), "a, b, c and d");
    }

    #[test]
    fn to_sentence_with_custom_separators() {
        assert_eq!(
            to_sentence_with(&["read", "write"], "/", " or "),
            "read or write"
        );
        assert_eq!(
            to_sentence_with(&["a", "b", "c"], "; ", "; finally "),
            "a; b; finally c"
        );
    }
}
