//! Constant-time byte and string comparison.

use sha2::{Digest, Sha256};
use std::hint::black_box;

/// Constant-time comparison of two byte sequences or strings.
///
/// Returns `true` only when `a` and `b` are identical in length and content.
///
/// Both inputs are first hashed with SHA-256, producing fixed 32-byte
/// digests, so the comparison loop always runs exactly 32 iterations
/// regardless of input length. This eliminates timing side-channels that
/// would leak input length from the loop bound or memory access pattern.
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
    let a_hash = Sha256::digest(a.as_ref());
    let b_hash = Sha256::digest(b.as_ref());

    let mut diff = 0u64;
    for i in 0..32 {
        let x = a_hash[i];
        let y = b_hash[i];
        diff |= (x ^ y) as u64;
    }
    black_box(diff) == 0
}

/// Constant-time comparison of two byte sequences or strings after SHA-256 hashing.
///
/// Hashes both inputs using SHA-256 and compares the fixed 32-byte digests in constant time.
/// This completely eliminates timing discrepancies from input length variance.
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
