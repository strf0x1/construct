//! Screen-width arithmetic for the TUI: how many columns a char or a
//! string takes on screen (an emoji pair can draw wider than its chars),
//! and the property-test text that breaks byte arithmetic. Byte-budget
//! cutting (`head`/`tail`) stays in `construct_core::text`.

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

/// Columns `s` takes on screen: its width without the control characters
/// a terminal draws as nothing (a ZWJ emoji stays one two-column glyph).
pub fn cols(s: &str) -> usize {
    unicode_width::UnicodeWidthStr::width(s.replace(|c: char| c.is_control(), "").as_str())
}

/// `s` cut to `w` columns, ending in `…` when it was cut.
pub fn fit(s: &str, w: usize) -> String {
    if unicode_width::UnicodeWidthStr::width(s) <= w {
        return s.to_string();
    }
    let mut out = String::new();
    let mut cw = 0;
    for c in s.chars() {
        let x = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if cw + x + 1 > w {
            break;
        }
        out.push(c);
        cw += x;
    }
    out.push('…');
    out
}

/// Property-test setup, duplicated from `construct_core::text` (`#[cfg(test)]`
/// items cannot cross crates): text that breaks byte arithmetic, and one
/// deterministic config so CI replays the same cases every run.
#[cfg(test)]
pub(crate) mod prop {
    use proptest::prelude::*;
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
        "é",
        "e\u{301}",
        "中文",
        "😀",
        "👨\u{200d}👩\u{200d}👧",
        "\u{200b}",
        "\u{feff}",
        "\u{3000}",
        "\u{a0}",
        "\r\n",
        "\n",
        "\r",
        "\n\n",
        "\t",
        " ",
        "  ",
        "\u{2028}",
        "\u{fe0f}",
        "ﷺ",
        "\u{0}",
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
    fn a_variation_selector_widens_the_char_before_it() {
        assert_eq!(cols_after(Some('0'), '\u{fe0f}'), 1);
        assert_eq!(cols_after(None, '中'), 2);
        assert_eq!(cols_after(Some('a'), '\t'), 0);
        assert_eq!(cols_after(Some('\0'), 'a'), 1);
    }
}
