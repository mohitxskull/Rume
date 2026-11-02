//! Constant-time byte and string comparison.

use std::hint::black_box;

/// Constant-time comparison of two byte sequences or strings.
///
/// Returns `true` only when `a` and `b` are identical in length and content.
///
/// Lengths are compared first (unequal lengths short-circuit to `false`).
/// Then every byte pair is XOR-accumulated without any early exit, so elapsed
/// time does not depend on how many bytes match or where they first differ.
///
/// `std::hint::black_box` is applied to the accumulator to prevent compiler
/// optimizations from short-circuiting or vectorizing the loop with early branches.
///
/// # Examples
///
/// ```
/// use rume::helpers::safe_equal;
///
/// assert!(safe_equal("secret_token", "secret_token"));
/// assert!(!safe_equal("secret_token", "wrong_token"));
/// assert!(safe_equal(b"raw_bytes", b"raw_bytes"));
/// ```
pub fn safe_equal<A, B>(a: A, b: B) -> bool
where
    A: AsRef<[u8]>,
    B: AsRef<[u8]>,
{
    let a_bytes = a.as_ref();
    let b_bytes = b.as_ref();

    if a_bytes.len() != b_bytes.len() {
        return false;
    }

    let mut diff = 0u8;
    for (x, y) in a_bytes.iter().zip(b_bytes.iter()) {
        diff |= x ^ y;
    }
    black_box(diff) == 0
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
        assert!(safe_equal(b"secret", b"secret"));
        assert!(safe_equal("mixed_types", b"mixed_types"));
    }

    #[test]
    fn safe_equal_different_returns_false() {
        assert!(!safe_equal("abc", "abd"));
        assert!(!safe_equal("same prefix", "same suffix"));
        assert!(!safe_equal(b"token_a", b"token_b"));
    }

    #[test]
    fn safe_equal_different_length_returns_false() {
        assert!(!safe_equal("abc", "abcd"));
        assert!(!safe_equal("", "a"));
        assert!(!safe_equal("aaaa", "aaab"));
        assert!(!safe_equal(b"short", b"longer_bytes"));
    }
}
