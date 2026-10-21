//! FNV-1a 64 over UTF-16 code units (low byte, then high byte), rendered as
//! a base-26 (a-z) digit stream. A displayed hash is a prefix of its line's
//! stream, so extending a colliding hash is a longer slice of the same
//! stream. UTF-16 matches the TypeScript reference implementation, so both
//! produce the same anchors for the same file.

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

pub const ALPHABET: &[u8; 26] = b"abcdefghijklmnopqrstuvwxyz";

/// 26^13 ≈ 2^61: 13 digits capture nearly the whole digest.
pub const MAX_HASH_LEN: usize = 13;

pub fn fnv1a64(input: &str) -> u64 {
    let mut h = FNV_OFFSET;
    for unit in input.encode_utf16() {
        h ^= u64::from(unit & 0xff);
        h = h.wrapping_mul(FNV_PRIME);
        h ^= u64::from(unit >> 8);
        h = h.wrapping_mul(FNV_PRIME);
    }
    h
}

/// The full digit stream for a digest, low digit first.
pub fn stream(digest: u64) -> String {
    let mut h = digest;
    let mut s = String::with_capacity(MAX_HASH_LEN);
    for _ in 0..MAX_HASH_LEN {
        s.push(ALPHABET[(h % 26) as usize] as char);
        h /= 26;
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_and_input_sensitive() {
        assert_eq!(fnv1a64("hello"), fnv1a64("hello"));
        assert_ne!(fnv1a64("hello"), fnv1a64("hello "));
        assert_ne!(fnv1a64("a"), fnv1a64("b"));
    }

    #[test]
    fn streams_are_max_len_lowercase() {
        for input in ["", "x", "return x;", "日本語テスト", "🎉 emoji"] {
            let s = stream(fnv1a64(input));
            assert_eq!(s.len(), MAX_HASH_LEN);
            assert!(s.bytes().all(|b| b.is_ascii_lowercase()), "{s}");
        }
    }

    /// Values from the TypeScript reference (`hashStream(fnv1a64(x))`), so a
    /// trace recorded under either implementation replays under the other.
    #[test]
    fn matches_the_reference_implementation() {
        for (input, want) in REFERENCE {
            assert_eq!(stream(fnv1a64(input)), *want, "{input:?}");
        }
    }

    const REFERENCE: &[(&str, &str)] = &[
        ("", "byzwakycplzzx"),
        ("x", "zkyrsxfnrtntg"),
        ("return x;", "iuzrffkygebch"),
        ("日本語テスト", "xfxoslpbgqhyv"),
        ("🎉 emoji", "zqkgclqjqkkyv"),
        ("}", "qszypaqztwfsg"),
        ("} 2", "wadtyzdctsvlv"),
    ];
}
