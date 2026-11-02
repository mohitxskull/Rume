//! Constant-time string comparison.

/// Constant-time comparison of two strings.
///
/// Returns `true` only when `a` and `b` are identical. Lengths are compared
/// first (unequal lengths short-circuit to `false`), then every byte pair is
/// XOR-accumulated without any early exit, so the elapsed time does not
/// depend on how many bytes match or where they first differ.
///
/// Why: an early-exit byte comparison leaks the position of the first
/// differing byte through timing. Rejecting different lengths up front is
/// unavoidable (length itself is public), but beyond that no branch depends
/// on secret data.
///
/// Revisit if: a constant-time length comparison becomes necessary.
pub fn safe_equal(a: &str, b: &str) -> bool {
    let a_bytes = a.as_bytes();
    let b_bytes = b.as_bytes();

    if a_bytes.len() != b_bytes.len() {
        return false;
    }

    let mut diff = 0u8;
    for (x, y) in a_bytes.iter().zip(b_bytes.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_equal_identical_returns_true() {
        assert!(safe_equal("", ""));
        assert!(safe_equal("abc", "abc"));
        assert!(safe_equal(
            "a very long secret value",
            "a very long secret value"
        ));
    }

    #[test]
    fn safe_equal_different_returns_false() {
        assert!(!safe_equal("abc", "abd"));
        assert!(!safe_equal("same prefix", "same suffix"));
    }

    #[test]
    fn safe_equal_different_length_returns_false() {
        assert!(!safe_equal("abc", "abcd"));
        assert!(!safe_equal("", "a"));
        assert!(!safe_equal("aaaa", "aaab"));
    }
}
