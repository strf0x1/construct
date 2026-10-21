//! Per-line anchor hashes for one file snapshot.
//!
//! Given a snapshot's lines, the map is deterministic (same content, same
//! map), unique (no two lines display the same hash, even identical ones:
//! duplicates are salted with their occurrence rank) and stable (a hash
//! depends on the line's normalized content, its rank among identical lines
//! and prefix-collision pressure, never on its line number, except the
//! last-resort salt).

use super::hash::{MAX_HASH_LEN, fnv1a64, stream};
use std::collections::HashMap;
use unicode_normalization::UnicodeNormalization;

pub struct AnchorMap {
    /// Displayed hash per line; index 0 is line 1.
    pub hashes: Vec<String>,
    /// Line contents, no EOL.
    pub lines: Vec<String>,
}

impl AnchorMap {
    pub fn len(&self) -> usize {
        self.lines.len()
    }
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }
    /// The anchor of a 1-based line, as the model sees it.
    pub fn anchor(&self, line: usize) -> String {
        format!("{line}:{}", self.hashes[line - 1])
    }
}

/// JavaScript's `\s`, so normalization matches the reference implementation.
fn js_space(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{0b}' | '\u{0c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}'
    )
}

/// NFC with all whitespace removed: anchors survive re-indentation and
/// formatter churn; any other change invalidates them.
pub fn normalize(line: &str) -> String {
    line.nfc().filter(|c| !js_space(*c)).collect()
}

pub fn build(lines: Vec<String>, min_hash_len: usize) -> AnchorMap {
    let norm: Vec<String> = lines.iter().map(|l| normalize(l)).collect();
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for n in &norm {
        *counts.entry(n).or_default() += 1;
    }

    // Identical lines (blank lines, lone braces) get their occurrence rank as
    // a salt. Rank, not line number: shifts that keep the duplicates' order
    // keep their anchors.
    let mut rank: HashMap<&str, usize> = HashMap::new();
    let mut streams: Vec<String> = norm
        .iter()
        .map(|n| {
            if counts[n.as_str()] > 1 {
                let r = rank.entry(n).or_default();
                *r += 1;
                stream(fnv1a64(&format!("{n} {r}")))
            } else {
                stream(fnv1a64(n))
            }
        })
        .collect();

    // Last resort: distinct keys colliding across the whole digest get their
    // line number as a salt. Astronomically rare.
    let mut by_stream: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, s) in streams.iter().enumerate() {
        by_stream.entry(s).or_default().push(i);
    }
    let collided: Vec<usize> = by_stream.into_values().filter(|g| g.len() > 1).flatten().collect();
    for i in collided {
        streams[i] = stream(fnv1a64(&format!("{} #{}", norm[i], i + 1)));
    }

    // Every hash starts at min_hash_len; each member of a colliding group
    // grows one digit at a time until every displayed hash is unique.
    let base = min_hash_len.clamp(2, MAX_HASH_LEN);
    let mut lens = vec![base; streams.len()];
    loop {
        let mut groups: HashMap<&str, Vec<usize>> = HashMap::new();
        for (i, s) in streams.iter().enumerate() {
            groups.entry(&s[..lens[i]]).or_default().push(i);
        }
        let mut extended = false;
        for i in groups.into_values().filter(|g| g.len() > 1).flatten() {
            if lens[i] < MAX_HASH_LEN {
                lens[i] += 1;
                extended = true;
            }
        }
        if !extended {
            break;
        }
    }

    let hashes = streams.iter().zip(&lens).map(|(s, &n)| s[..n].to_string()).collect();
    AnchorMap { hashes, lines }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Anchor {
    pub line: usize,
    pub hash: String,
}

/// Parse `LINE:HASH`. Tolerates an echoed read row (`12:ax|content`), stray
/// whitespace and case, never a missing or malformed hash: the hash is the
/// safety check.
pub fn parse(raw: &str) -> Option<Anchor> {
    let head = raw.split_once('|').map_or(raw, |(h, _)| h).to_lowercase();
    let (line, hash) = head.split_once(':')?;
    let (line, hash) = (line.trim(), hash.trim());
    if line.is_empty() || !line.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if !(2..=MAX_HASH_LEN).contains(&hash.len()) || !hash.bytes().all(|b| b.is_ascii_lowercase()) {
        return None;
    }
    let line: usize = line.parse().ok().filter(|&n| n >= 1)?;
    Some(Anchor { line, hash: hash.to_string() })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    OutOfRange,
    Mismatch,
}

impl Failure {
    pub fn code(self) -> &'static str {
        match self {
            Failure::OutOfRange => "LINE_OUT_OF_RANGE",
            Failure::Mismatch => "ANCHOR_MISMATCH",
        }
    }
}

/// Check an anchor against the current map. Exact hash match only: a right
/// hash on the wrong line is a mismatch, never a relocation.
pub fn check(map: &AnchorMap, a: &Anchor) -> Result<(), (Failure, String)> {
    if a.line > map.len() {
        return Err((Failure::OutOfRange, format!("line {} does not exist (file has {} lines)", a.line, map.len())));
    }
    let current = &map.hashes[a.line - 1];
    if *current != a.hash {
        return Err((
            Failure::Mismatch,
            format!(
                "line {} does not match hash \"{}\" (current: \"{current}\"); the content you targeted is not at line {}",
                a.line, a.hash, a.line
            ),
        ));
    }
    Ok(())
}

#[cfg(test)]
pub(super) fn map(lines: &[&str]) -> AnchorMap {
    build(lines.iter().map(|s| s.to_string()).collect(), 2)
}

#[cfg(test)]
mod tests {
    use super::super::hash::{fnv1a64, stream};
    use super::*;
    use std::collections::HashSet;

    fn anchor(line: usize, hash: &str) -> Anchor {
        Anchor { line, hash: hash.into() }
    }

    #[test]
    fn normalize_strips_all_whitespace_and_applies_nfc() {
        assert_eq!(normalize("  a b\tc  "), "abc");
        assert_eq!(normalize("caf\u{e9}"), normalize("cafe\u{301}"));
        assert_eq!(normalize("\t\u{3000}\u{a0} "), "");
    }

    #[test]
    fn hashes_are_unique_even_with_heavy_duplication() {
        let lines = ["", "}", "x++;", "", "}", "x++;", "", "}", "x++;", "function f() {", "}"];
        let m = map(&lines);
        assert_eq!(m.hashes.iter().collect::<HashSet<_>>().len(), lines.len());
    }

    #[test]
    fn maps_are_deterministic() {
        let lines = ["alpha", "", "beta", "", "gamma"];
        assert_eq!(map(&lines).hashes, map(&lines).hashes);
    }

    #[test]
    fn indentation_only_changes_keep_the_hash() {
        let a = map(&["if (x) {", "return 1;", "}"]);
        let b = map(&["if (x) {", "    return 1;", "}"]);
        assert_eq!(a.hashes[1], b.hashes[1]);
    }

    #[test]
    fn appending_below_keeps_earlier_anchors() {
        let before = map(&["alpha", "}", "beta", "}"]);
        let after = map(&["alpha", "}", "beta", "}", "omega"]);
        assert_eq!(after.hashes[..4], before.hashes[..]);
    }

    #[test]
    fn inserting_above_makes_old_anchors_fail_loudly() {
        let before = map(&["}", "alpha", "}"]);
        let after = map(&["inserted", "}", "alpha", "}"]);
        let h = &before.hashes[1];
        assert_eq!(check(&after, &anchor(2, h)).unwrap_err().0, Failure::Mismatch);
        assert!(check(&after, &anchor(3, h)).is_ok(), "the content moved to line 3 with its hash");
    }

    #[test]
    fn prefix_collisions_extend_the_colliding_group() {
        let mut seen: HashMap<String, String> = HashMap::new();
        let mut pair = None;
        for i in 0..20000 {
            let content = format!("line_{i}");
            let prefix = stream(fnv1a64(&content))[..2].to_string();
            if let Some(other) = seen.get(&prefix) {
                pair = Some((other.clone(), content));
                break;
            }
            seen.insert(prefix, content);
        }
        let (a, b) = pair.expect("26^2 buckets must collide");
        let m = map(&[&a, &b, "unrelated content"]);
        assert_ne!(m.hashes[0], m.hashes[1]);
        assert!(m.hashes[0].len() > 2 && m.hashes[1].len() > 2, "the colliding group grows");
        assert_eq!(m.hashes.iter().collect::<HashSet<_>>().len(), 3);
    }

    #[test]
    fn parse_tolerates_echoes_and_case_but_not_garbage() {
        assert_eq!(parse("12:ax"), Some(anchor(12, "ax")));
        assert_eq!(parse("  12 : AX  "), Some(anchor(12, "ax")));
        assert_eq!(parse("12:ax|return x;"), Some(anchor(12, "ax")));
        for bad in ["12", "12:a", "0:ax", "12:ax9", ":ax", "-1:ax", "1:abcdefghijklmn"] {
            assert_eq!(parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn check_reports_out_of_range_and_mismatch() {
        let m = map(&["a", "b"]);
        assert_eq!(check(&m, &anchor(3, "aa")).unwrap_err().0, Failure::OutOfRange);
        assert_eq!(check(&m, &anchor(1, "zz")).unwrap_err().0, Failure::Mismatch);
        assert!(check(&m, &anchor(1, &m.hashes[0])).is_ok());
    }

    #[test]
    fn a_duplicate_hash_names_its_occurrence() {
        let m = map(&["}", "x", "}"]);
        assert_eq!(check(&m, &anchor(3, &m.hashes[0])).unwrap_err().0, Failure::Mismatch);
    }

    /// Whole maps from the TypeScript reference, covering rank salts,
    /// NFC, full-width spaces and a shift by one inserted line.
    #[test]
    fn matches_the_reference_implementation() {
        let m =
            map(&["fn main() {", "    let x = 1;", "", "    println!(\"{x}\");", "", "}", "café", "  a\u{3000}b  "]);
        assert_eq!(m.hashes, ["gr", "jv", "wt", "rb", "rz", "qs", "wd", "sm"]);
        assert_eq!(map(&["alpha", "beta", "gamma", "delta"]).hashes, ["jk", "ds", "ab", "ld"]);
        assert_eq!(map(&["inserted", "alpha", "beta", "gamma", "delta"]).hashes, ["vp", "jk", "ds", "ab", "ld"]);
    }
}
