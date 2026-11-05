//! Ordinal number formatting (`1st`, `2nd`, `3rd`, etc.).

/// Returns the English ordinal suffix (`"st"`, `"nd"`, `"rd"`, or `"th"`) for a number.
///
/// # Examples
///
/// ```
/// use rume::helpers::ordinal_suffix;
///
/// assert_eq!(ordinal_suffix(1), "st");
/// assert_eq!(ordinal_suffix(2), "nd");
/// assert_eq!(ordinal_suffix(3), "rd");
/// assert_eq!(ordinal_suffix(4), "th");
/// assert_eq!(ordinal_suffix(11), "th");
/// assert_eq!(ordinal_suffix(21), "st");
/// ```
pub fn ordinal_suffix(n: u64) -> &'static str {
    let rem100 = n % 100;
    if (11..=13).contains(&rem100) {
        return "th";
    }

    match n % 10 {
        1 => "st",
        2 => "nd",
        3 => "rd",
        _ => "th",
    }
}

/// Converts a number into its ordinal string representation.
///
/// # Examples
///
/// ```
/// use rume::helpers::ordinal;
///
/// assert_eq!(ordinal(1), "1st");
/// assert_eq!(ordinal(2), "2nd");
/// assert_eq!(ordinal(3), "3rd");
/// assert_eq!(ordinal(11), "11th");
/// assert_eq!(ordinal(21), "21st");
/// assert_eq!(ordinal(102), "102nd");
/// ```
pub fn ordinal(n: u64) -> String {
    format!("{n}{}", ordinal_suffix(n))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinal_suffix_handles_irregular_teens() {
        assert_eq!(ordinal_suffix(11), "th");
        assert_eq!(ordinal_suffix(12), "th");
        assert_eq!(ordinal_suffix(13), "th");
        assert_eq!(ordinal_suffix(111), "th");
        assert_eq!(ordinal_suffix(112), "th");
        assert_eq!(ordinal_suffix(113), "th");
    }

    #[test]
    fn ordinal_suffix_standard_cases() {
        assert_eq!(ordinal_suffix(0), "th");
        assert_eq!(ordinal_suffix(1), "st");
        assert_eq!(ordinal_suffix(2), "nd");
        assert_eq!(ordinal_suffix(3), "rd");
        assert_eq!(ordinal_suffix(4), "th");
        assert_eq!(ordinal_suffix(21), "st");
        assert_eq!(ordinal_suffix(22), "nd");
        assert_eq!(ordinal_suffix(23), "rd");
        assert_eq!(ordinal_suffix(24), "th");
    }

    #[test]
    fn ordinal_formatting() {
        assert_eq!(ordinal(1), "1st");
        assert_eq!(ordinal(22), "22nd");
        assert_eq!(ordinal(103), "103rd");
    }
}
