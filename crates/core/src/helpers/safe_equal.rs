//! Constant-time byte and string comparison.

use subtle::ConstantTimeEq;

/// Constant-time comparison of two byte sequences or strings.
///
/// Returns `true` only when `a` and `b` are identical in length and content.
///
/// The comparison delegates to [`subtle::ConstantTimeEq::ct_eq`], which is
/// audited constant-time crypto code. Equal-length inputs are compared
/// byte-by-byte without short-circuiting, so no data-dependent branch or
/// memory-access pattern leaks byte content. When the lengths differ, the
/// comparison returns `false` without iterating over the longer input, so the
/// runtime reveals only that the lengths are unequal — never how much they
/// differ.
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
    // Why: `subtle::ConstantTimeEq` is audited, constant-time crypto code — no
    // hand-rolled loop, no length-dependent timing side-channel from loop bounds
    // or SHA-256 pre-hashing.
    bool::from(a.as_ref().ct_eq(b.as_ref()))
}

/// Constant-time comparison of two byte sequences or strings.
///
/// `safe_equal_hashed` is preserved for API compatibility. It now delegates
/// directly to [`safe_equal`]: because `subtle::ConstantTimeEq` compares
/// fixed-length slices in constant time and handles unequal lengths without
/// leaking their magnitude, the separate SHA-256 pre-hashing step the previous
/// implementation used is no longer necessary.
///
/// # Examples
///
/// ```
/// use rume::helpers::safe_equal_hashed;
///
/// assert!(safe_equal_hashed("secret_token", "secret_token"));
/// assert!(!safe_equal_hashed("secret_token", "wrong_token"));
/// ```
pub fn safe_equal_hashed<A, B>(a: A, b: B) -> bool
where
    A: AsRef<[u8]>,
    B: AsRef<[u8]>,
{
    safe_equal(a, b)
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

    #[test]
    fn safe_equal_hashed_tests() {
        assert!(safe_equal_hashed("secret", "secret"));
        assert!(safe_equal_hashed(b"token", b"token"));
        assert!(!safe_equal_hashed("secret", "other"));
        assert!(!safe_equal_hashed("short", "longer_value"));
    }
}
