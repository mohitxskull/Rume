//! Constant-time byte and string comparison.

use sha2::{Digest, Sha256};
use std::hint::black_box;

/// Constant-time comparison of two byte sequences or strings.
///
/// Returns `true` only when `a` and `b` are identical in length and content.
///
/// Unlike naive comparisons, lengths are not short-circuited with early exits.
/// The comparison iterates over the maximum length of both slices, folding
/// any byte differences and length discrepancies into a single accumulator.
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

    let max_len = a_bytes.len().max(b_bytes.len());
    let mut diff = (a_bytes.len() ^ b_bytes.len()) as u64;

    for i in 0..max_len {
        let x = a_bytes.get(i).copied().unwrap_or(0);
        let y = b_bytes.get(i).copied().unwrap_or(0);
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
    let a_hash = Sha256::digest(a.as_ref());
    let b_hash = Sha256::digest(b.as_ref());
    safe_equal(a_hash, b_hash)
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
