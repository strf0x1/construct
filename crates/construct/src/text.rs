//! Cutting a string to a byte budget. A byte offset lands inside a multibyte
//! character as often as not, and slicing there panics (a non-ASCII date did,
//! once), so every cap rounds to a char boundary here instead of by hand.

/// The longest prefix of `s` at most `max` bytes long.
pub fn head(s: &str, max: usize) -> &str {
    s.split_at(s.floor_char_boundary(max)).0
}

/// The longest suffix of `s` at most `max` bytes long.
pub fn tail(s: &str, max: usize) -> &str {
    s.split_at(s.ceil_char_boundary(s.len().saturating_sub(max))).1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn head_never_splits_a_char() {
        let s = "aé€😀"; // 1, 2, 3 and 4 bytes
        let cuts: Vec<&str> = (0..=11).map(|n| head(s, n)).collect();
        assert_eq!(cuts, ["", "a", "a", "aé", "aé", "aé", "aé€", "aé€", "aé€", "aé€", "aé€😀", "aé€😀"]);
    }

    #[test]
    fn tail_never_splits_a_char() {
        let s = "😀€éa";
        let cuts: Vec<&str> = (0..=11).map(|n| tail(s, n)).collect();
        assert_eq!(cuts, ["", "a", "a", "éa", "éa", "éa", "€éa", "€éa", "€éa", "€éa", "😀€éa", "😀€éa"]);
    }
}
