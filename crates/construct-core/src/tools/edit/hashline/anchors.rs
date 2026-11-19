//! Per-line anchor hashes for one file snapshot.
//!
//! Given a snapshot's lines, the map is deterministic (same content, same
//! map), unique (no two lines display the same hash, even identical ones:
//! duplicates are salted with their occurrence rank, and under
//! `salt = "count"` with the number of copies too) and stable (a hash
//! depends on the line's normalized content, its rank among identical lines
//! and prefix-collision pressure, never on its line number, except the
//! last-resort salt).
//!
//! Under `anchor = "hash"` the model sees and sends the hash alone, and it
//! resolves to whichever line shows that hash now.

use super::hash::{MAX_HASH_LEN, fnv1a64, stream};
use crate::config::{AnchorMode, Salt};
use std::collections::HashMap;
use unicode_normalization::UnicodeNormalization;

#[derive(Debug)]
pub struct AnchorMap {
    /// Displayed hash per line; index 0 is line 1.
    pub hashes: Vec<String>,
    /// Line contents, no EOL.
    pub lines: Vec<String>,
    pub mode: AnchorMode,
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
        match self.mode {
            AnchorMode::LineHash => format!("{line}:{}", self.hashes[line - 1]),
            AnchorMode::Hash => self.hashes[line - 1].clone(),
        }
    }
    /// A line as read output shows it: `anchor|content`.
    pub fn row(&self, line: usize) -> String {
        format!("{}|{}", self.anchor(line), self.lines[line - 1])
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

pub fn build(lines: Vec<String>, min_hash_len: usize, mode: AnchorMode, salt: Salt) -> AnchorMap {
    let norm: Vec<String> = lines.iter().map(|l| normalize(l)).collect();
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for n in &norm {
        *counts.entry(n).or_default() += 1;
    }

    // Identical lines (blank lines, lone braces) get their occurrence rank as
    // a salt. Rank, not line number: shifts that keep the duplicates' order
    // keep their anchors. The count salt also names how many copies there
    // are, so adding or removing one re-hashes every copy.
    let mut rank: HashMap<&str, usize> = HashMap::new();
    let mut streams: Vec<String> = norm
        .iter()
        .map(|n| {
            if counts[n.as_str()] > 1 {
                let r = rank.entry(n).or_default();
                *r += 1;
                match salt {
                    Salt::Rank => stream(fnv1a64(&format!("{n} {r}"))),
                    Salt::Count => stream(fnv1a64(&format!("{n} {r}/{}", counts[n.as_str()]))),
                }
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
            groups.entry(shown(s, lens[i])).or_default().push(i);
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

    let hashes = streams.iter().zip(&lens).map(|(s, &n)| shown(s, n).to_string()).collect();
    AnchorMap { hashes, lines, mode }
}

/// The first `n` letters of a hash stream, the part a read shows.
fn shown(stream: &str, n: usize) -> &str {
    stream.get(..n).unwrap_or(stream) // streams are ASCII letters
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

/// A bare `HASH` (or an echoed `HASH|content`), for `anchor = "hash"`.
pub fn parse_hash(raw: &str) -> Option<String> {
    let head = raw.split_once('|').map_or(raw, |(h, _)| h).trim().to_lowercase();
    ((2..=MAX_HASH_LEN).contains(&head.len()) && head.bytes().all(|b| b.is_ascii_lowercase())).then_some(head)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    Bad,
    OutOfRange,
    Mismatch,
    /// Hash mode: no line shows this hash now.
    NotFound,
}

impl Failure {
    pub fn code(self) -> &'static str {
        match self {
            Failure::Bad => "BAD_ANCHOR",
            Failure::OutOfRange => "LINE_OUT_OF_RANGE",
            Failure::Mismatch => "ANCHOR_MISMATCH",
            Failure::NotFound => "ANCHOR_NOT_FOUND",
        }
    }
}

/// Why an anchor didn't resolve, and the line worth showing around it.
pub struct Unresolved {
    pub failure: Failure,
    pub message: String,
    pub window_line: Option<usize>,
}

/// Resolve an anchor string to a 1-based line under the map's mode.
pub fn resolve(map: &AnchorMap, raw: &str) -> Result<usize, Unresolved> {
    match map.mode {
        AnchorMode::LineHash => {
            let a = parse(raw).ok_or_else(|| Unresolved {
                failure: Failure::Bad,
                message: format!("\"{raw}\" is not a valid anchor; expected \"LINE:HASH\" like \"12:ax\", copied exactly from the latest read/edit output"),
                window_line: None,
            })?;
            check(map, &a).map(|()| a.line).map_err(|(failure, message)| Unresolved {
                failure,
                message,
                window_line: Some(a.line.min(map.len().max(1))),
            })
        }
        AnchorMode::Hash => {
            let hash = parse_hash(raw).ok_or_else(|| Unresolved {
                failure: Failure::Bad,
                message: format!("\"{raw}\" is not a valid anchor; expected \"HASH\" like \"ax\", copied exactly from the latest read/edit output"),
                window_line: None,
            })?;
            match map.hashes.iter().position(|h| *h == hash) {
                Some(i) => Ok(i + 1),
                None => Err(Unresolved {
                    failure: Failure::NotFound,
                    message: format!(
                        "no line has hash \"{hash}\" now; the content you targeted was changed or removed"
                    ),
                    window_line: None,
                }),
            }
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
    build(lines.iter().map(|s| s.to_string()).collect(), 2, AnchorMode::LineHash, Salt::Rank)
}

#[cfg(test)]
pub(super) fn hash_map(lines: &[&str]) -> AnchorMap {
    build(lines.iter().map(|s| s.to_string()).collect(), 2, AnchorMode::Hash, Salt::Rank)
}

#[cfg(test)]
fn counted(lines: &[&str], mode: AnchorMode) -> AnchorMap {
    build(lines.iter().map(|s| s.to_string()).collect(), 2, mode, Salt::Count)
}

#[cfg(test)]
pub(super) mod props {
    use super::*;
    use crate::text::prop;
    use proptest::prelude::*;

    /// A file's lines, with the repeats (blank lines, braces) that salting
    /// exists for.
    pub(in super::super) fn lines() -> impl Strategy<Value = Vec<String>> {
        let line = prop_oneof![
            3 => prop::text().prop_map(|t| t.replace(['\n', '\r'], "")),
            1 => proptest::sample::select(&["", "}", "  }", "x"][..]).prop_map(String::from),
        ];
        proptest::collection::vec(line, 0..16)
    }

    pub(in super::super) fn map() -> impl Strategy<Value = AnchorMap> {
        (lines(), 2usize..4, any::<bool>(), any::<bool>()).prop_map(|(lines, n, hash, count)| {
            let mode = if hash { AnchorMode::Hash } else { AnchorMode::LineHash };
            build(lines, n, mode, if count { Salt::Count } else { Salt::Rank })
        })
    }

    proptest! {
        #![proptest_config(prop::config())]

        /// Every line's anchor, bare or echoed as a whole read row, resolves
        /// back to that line, under either mode and salt.
        #[test]
        fn every_anchor_resolves_to_its_own_line(m in map()) {
            for line in 1..=m.len() {
                prop_assert_eq!(resolve(&m, &m.anchor(line)).ok(), Some(line), "{}", m.anchor(line));
                prop_assert_eq!(resolve(&m, &m.row(line)).ok(), Some(line), "{}", m.row(line));
            }
        }

        #[test]
        fn resolving_any_text_never_panics(m in map(), raw in prop::text_with(&["12:ab", ":", "|", "0:", "99999999999999999999:ab", "AB"])) {
            let _ = resolve(&m, &raw);
        }
    }
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
            let prefix: String = stream(fnv1a64(&content)).chars().take(2).collect();
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

    #[test]
    fn parse_hash_takes_a_bare_hash_or_an_echoed_row() {
        assert_eq!(parse_hash(" AX "), Some("ax".into()));
        assert_eq!(parse_hash("ax|return x;"), Some("ax".into()));
        for bad in ["a", "12:ax", "ax9", "", "abcdefghijklmn"] {
            assert_eq!(parse_hash(bad), None, "{bad}");
        }
    }

    #[test]
    fn line_hash_resolution_never_relocates() {
        let m = map(&["inserted", "alpha", "beta"]);
        let old = map(&["alpha", "beta"]).anchor(2);
        let u = resolve(&m, &old).err().unwrap();
        assert_eq!((u.failure, u.window_line), (Failure::Mismatch, Some(2)));
        assert_eq!(resolve(&m, &m.anchor(3)).ok(), Some(3));
        assert_eq!(resolve(&m, "ax").err().unwrap().failure, Failure::Bad);
    }

    #[test]
    fn hash_resolution_follows_content_and_fails_when_it_is_gone() {
        let old = hash_map(&["alpha", "beta"]);
        let moved = hash_map(&["inserted", "alpha", "beta"]);
        assert_eq!(resolve(&moved, &old.anchor(2)).ok(), Some(3));
        let changed = hash_map(&["alpha", "BETA"]);
        assert_eq!(resolve(&changed, &old.anchor(2)).err().unwrap().failure, Failure::NotFound);
        assert_eq!(resolve(&changed, &old.anchor(1)).ok(), Some(1));
        assert_eq!(resolve(&changed, "1:ax").err().unwrap().failure, Failure::Bad);
    }

    /// The hazard of hash-only anchors under the rank salt: a duplicate's hash names its
    /// rank, so a new duplicate above silently moves which line it names.
    /// (Going from one copy to two re-salts them all, which is loud.)
    #[test]
    fn hash_mode_relocates_a_duplicate_when_one_is_added_above() {
        let old = hash_map(&["if (a) {", "}", "if (b) {", "}"]);
        let new = hash_map(&["}", "if (a) {", "}", "if (b) {", "}"]);
        assert_eq!(resolve(&new, &old.anchor(4)).ok(), Some(3), "names if (a)'s brace, not line 5");
        let new = hash_map(&["}", "if (a) {", "}"]);
        assert_eq!(resolve(&new, &hash_map(&["if (a) {", "}"]).anchor(2)).ok(), None, "one to two copies is loud");
    }

    /// line+hash has the same hole in one geometry: a block holding a new
    /// copy, inserted above, exactly as tall as the gap between two copies.
    /// The previous copy then lands on the stale line with the stale rank, so
    /// the stale anchor checks out and names the wrong line.
    #[test]
    fn line_hash_accepts_a_stale_duplicate_when_the_insert_matches_the_gap() {
        let old = map(&["def f(u):", "    if u.banned:", "        return None", "    audit(u)", "    return None"]);
        let new = map(&[
            "def f(u):",
            "    if u.admin:",
            "        return None",
            "    if u.banned:",
            "        return None",
            "    audit(u)",
            "    return None",
        ]);
        assert_eq!(resolve(&new, &old.anchor(5)).ok(), Some(5), "the banned branch, not line 7");
        // One line taller than the gap and the same anchor is rejected.
        let taller = map(&[
            "def f(u):",
            "    # admins skip the audit",
            "    if u.admin:",
            "        return None",
            "    if u.banned:",
            "        return None",
            "    audit(u)",
            "    return None",
        ]);
        assert_eq!(resolve(&taller, &old.anchor(5)).err().unwrap().failure, Failure::Mismatch);
    }

    /// The count salt closes #26: the new copy above re-hashes every copy,
    /// so the stale anchor is gone instead of naming if (a)'s brace.
    #[test]
    fn count_salt_rejects_a_stale_duplicate_in_hash_mode() {
        let old = counted(&["if (a) {", "}", "if (b) {", "}"], AnchorMode::Hash);
        let new = counted(&["}", "if (a) {", "}", "if (b) {", "}"], AnchorMode::Hash);
        assert_eq!(resolve(&new, &old.anchor(4)).err().unwrap().failure, Failure::NotFound);
        assert_eq!(resolve(&new, &old.anchor(2)).err().unwrap().failure, Failure::NotFound);
        let old = counted(&["if (a) {", "}"], AnchorMode::Hash);
        let new = counted(&["}", "if (a) {", "}"], AnchorMode::Hash);
        assert_eq!(resolve(&new, &old.anchor(2)).ok(), None, "one to two copies is still loud");
    }

    /// And the gap geometry under line+hash: the previous copy lands on the
    /// stale line with the stale rank, but not the stale count.
    #[test]
    fn count_salt_rejects_the_gap_case_in_line_hash_mode() {
        let old = counted(
            &["def f(u):", "    if u.banned:", "        return None", "    audit(u)", "    return None"],
            AnchorMode::LineHash,
        );
        let new = counted(
            &[
                "def f(u):",
                "    if u.admin:",
                "        return None",
                "    if u.banned:",
                "        return None",
                "    audit(u)",
                "    return None",
            ],
            AnchorMode::LineHash,
        );
        assert_eq!(resolve(&new, &old.anchor(5)).err().unwrap().failure, Failure::Mismatch);
        assert_eq!(resolve(&new, &new.anchor(7)).ok(), Some(7));
    }

    /// The hole the count salt leaves: one copy removed and another added
    /// between a read and an edit keeps the count, so ranks shift silently.
    #[test]
    fn count_salt_still_relocates_when_the_count_is_unchanged() {
        let old = counted(&["if (a) {", "}", "if (b) {", "}"], AnchorMode::Hash);
        let new = counted(&["if (a) {", "if (b) {", "}", "if (c) {", "}"], AnchorMode::Hash);
        assert_eq!(resolve(&new, &old.anchor(4)).ok(), Some(5), "names if (c)'s brace, not if (b)'s on line 3");
    }

    #[test]
    fn count_salt_leaves_unique_lines_and_the_rank_default_alone() {
        let lines = ["fn main() {", "", "    let x = 1;", "", "}"];
        let (rank, count) = (map(&lines), counted(&lines, AnchorMode::LineHash));
        for i in [0, 2, 4] {
            assert_eq!(rank.hashes[i], count.hashes[i], "line {}", i + 1);
        }
        assert_ne!((&rank.hashes[1], &rank.hashes[3]), (&count.hashes[1], &count.hashes[3]));
        assert_eq!(crate::config::HashlineConfig::default().salt, Salt::Rank);
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
