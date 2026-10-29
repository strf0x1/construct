//! Cutting a string to a byte budget. A byte offset lands inside a multibyte
//! character as often as not, and slicing there panics (a non-ASCII date did,
//! once), so every cap rounds to a char boundary here instead of by hand.

/// Columns `c` takes on screen after `prev`, for wrapping one char at a
/// time. Its own width mostly, but a pair can draw wider than its chars
/// (`0` and U+FE0F is a two-column emoji), and a control character draws
/// as nothing.
pub fn cols_after(prev: Option<char>, c: char) -> usize {
    use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
    if c.is_control() {
        return 0;
    }
    let Some(p) = prev.filter(|p| !p.is_control()) else { return c.width().unwrap_or(0) };
    let mut b = [0u8; 8];
    let pl = p.encode_utf8(&mut b).len();
    let cl = c.encode_utf8(&mut b[pl..]).len();
    let pair = std::str::from_utf8(&b[..pl + cl]).map_or(0, UnicodeWidthStr::width);
    pair.saturating_sub(p.width().unwrap_or(0))
}

/// The longest prefix of `s` at most `max` bytes long.
pub fn head(s: &str, max: usize) -> &str {
    s.split_at(s.floor_char_boundary(max)).0
}

/// The longest suffix of `s` at most `max` bytes long.
pub fn tail(s: &str, max: usize) -> &str {
    s.split_at(s.ceil_char_boundary(s.len().saturating_sub(max))).1
}

/// Shared property-test setup: text that breaks byte arithmetic, and one
/// deterministic config so CI replays the same cases every run.
#[cfg(test)]
pub(crate) mod prop {
    use proptest::prelude::*;
    pub(crate) use proptest::sample::Index;
    use proptest::test_runner::{Config, RngSeed};

    /// 256 cases from a fixed seed, no regression files written. A failure
    /// is pinned as a unit test from the shrunk input instead.
    /// `PROPTEST_CASES` and `PROPTEST_RNG_SEED` still override, to search wider.
    pub(crate) fn config() -> Config {
        let mut c = Config::default();
        if std::env::var_os("PROPTEST_RNG_SEED").is_none() {
            c.rng_seed = RngSeed::Fixed(0x636f_6e73_7472_7563);
        }
        c.failure_persistence = None;
        c
    }

    /// Pieces that have broken text handling before, or would: multibyte,
    /// combining, wide, zero-width and emoji sequences, every line ending,
    /// tabs and odd spaces.
    const NASTY: &[&str] = &[
        "é", "e\u{301}", "中文", "😀", "👨\u{200d}👩\u{200d}👧", "\u{200b}", "\u{feff}", "\u{3000}", "\u{a0}",
        "\r\n", "\n", "\r", "\n\n", "\t", " ", "  ", "\u{2028}", "\u{fe0f}", "ﷺ", "\u{0}",
    ];

    /// Arbitrary text, weighted towards `NASTY`, ASCII words and `extra`
    /// (a target's own syntax), with now and then a line far wider than any
    /// screen.
    pub(crate) fn text_with(extra: &'static [&'static str]) -> impl Strategy<Value = String> {
        let extra = if extra.is_empty() { NASTY } else { extra };
        let piece = prop_oneof![
            3 => any::<char>().prop_map(String::from),
            3 => proptest::sample::select(NASTY).prop_map(String::from),
            3 => proptest::sample::select(extra).prop_map(String::from),
            3 => "[a-z]{1,10} ?",
            1 => (1usize..400, proptest::sample::select(&["x", "中", "é", "ab "][..])).prop_map(|(n, s)| s.repeat(n)),
        ];
        proptest::collection::vec(piece, 0..48).prop_map(|v| v.concat())
    }

    pub(crate) fn text() -> impl Strategy<Value = String> {
        text_with(&[])
    }

    /// Columns a line takes on screen: where ratatui's cursor stops after
    /// drawing it. A string's `width()` is not that: it counts a control
    /// character as a column, which ratatui drops.
    pub(crate) fn cols(line: &ratatui::text::Line) -> usize {
        let area = ratatui::layout::Rect::new(0, 0, u16::MAX / 4, 1);
        let mut buf = ratatui::buffer::Buffer::empty(area);
        usize::from(buf.set_line(0, 0, line, area.width).0)
    }
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

    proptest::proptest! {
        #![proptest_config(prop::config())]

        #[test]
        fn head_and_tail_are_a_prefix_and_suffix_within_budget(s in prop::text(), max in 0usize..64) {
            let (h, t) = (head(&s, max), tail(&s, max));
            proptest::prop_assert!(h.len() <= max && s.starts_with(h));
            proptest::prop_assert!(t.len() <= max && s.ends_with(t));
            // Longest: one more char would go over.
            proptest::prop_assert!(s.get(h.len()..).and_then(|r| r.chars().next()).is_none_or(|c| h.len() + c.len_utf8() > max));
        }
    }

    #[test]
    fn a_variation_selector_widens_the_char_before_it() {
        assert_eq!(cols_after(Some('0'), '\u{fe0f}'), 1);
        assert_eq!(cols_after(None, '中'), 2);
        assert_eq!(cols_after(Some('a'), '\t'), 0);
        assert_eq!(cols_after(Some('\0'), 'a'), 1);
    }

    #[test]
    fn tail_never_splits_a_char() {
        let s = "😀€éa";
        let cuts: Vec<&str> = (0..=11).map(|n| tail(s, n)).collect();
        assert_eq!(cuts, ["", "a", "a", "éa", "éa", "éa", "€éa", "€éa", "€éa", "€éa", "😀€éa", "😀€éa"]);
    }
}
