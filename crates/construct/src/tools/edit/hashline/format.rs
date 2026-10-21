//! Rendering: anchored rows, recovery windows, and the edit tool's success
//! and rejection text. Weak models are the audience, so every error carries
//! the fresh anchors needed to fix it in one turn.

use super::anchors::AnchorMap;
use super::apply::Region;
use super::ops::{Kind, OpError};
use std::collections::BTreeSet;

/// Lines from..=to (1-based, clamped) as anchored rows.
pub fn rows(map: &AnchorMap, from: usize, to: usize) -> Vec<String> {
    (from.max(1)..=to.min(map.len())).map(|n| map.row(n)).collect()
}

/// Windows of `radius` lines around each center, merged where they touch.
pub fn windows(map: &AnchorMap, centers: &[usize], radius: usize) -> String {
    if map.is_empty() {
        return "(file is empty)".into();
    }
    let mut spans: Vec<(usize, usize)> =
        centers.iter().map(|&c| (c.saturating_sub(radius).max(1), (c + radius).min(map.len()))).collect();
    spans.sort();
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for (a, b) in spans {
        match merged.last_mut() {
            Some(last) if a <= last.1 + 1 => last.1 = last.1.max(b),
            _ => merged.push((a, b)),
        }
    }
    merged.iter().map(|&(a, b)| rows(map, a, b).join("\n")).collect::<Vec<_>>().join("\n…\n")
}

fn describe(r: &Region) -> String {
    let span = |a: usize, b: usize| if a == b { format!("line {a}") } else { format!("lines {a}-{b}") };
    match r.kind {
        Kind::Set | Kind::ReplaceRange => {
            format!("replaced {} (now {})", span(r.old_start, r.old_end), span(r.new_start, r.new_end))
        }
        Kind::InsertAfter | Kind::InsertBefore => {
            format!("inserted {} line(s) at {}", r.new_end + 1 - r.new_start, span(r.new_start, r.new_end))
        }
        Kind::Delete | Kind::DeleteRange => format!("deleted {}", span(r.old_start, r.old_end)),
    }
}

pub fn success(
    path: &str,
    regions: &[Region],
    new: &AnchorMap,
    old_len: usize,
    delta: isize,
    context: usize,
) -> String {
    let mut parts = vec![format!(
        "ok: applied {} edit(s) to {path}: {}.",
        regions.len(),
        regions.iter().map(describe).collect::<Vec<_>>().join("; ")
    )];
    if delta != 0 {
        parts.push(format!(
            "{path} is now {} lines (was {old_len}); anchors below the first change shifted.",
            new.len()
        ));
    }
    let mut centers = vec![];
    for r in regions {
        if r.new_end >= r.new_start {
            centers.extend(r.new_start..=r.new_end);
        } else {
            // A pure delete: show the seam.
            centers.push(r.new_start.saturating_sub(1).max(1));
            centers.push(r.new_start.min(new.len()));
        }
    }
    if new.is_empty() {
        parts.push(format!("{path} is now empty."));
    } else if !centers.is_empty() {
        parts.push("fresh anchors for the edited region (use these for follow-up edits, or re-read):".into());
        parts.push(windows(new, &centers, context));
    }
    parts.join("\n")
}

pub fn rejection(path: &str, total: usize, errors: &[OpError], map: &AnchorMap, radius: usize) -> String {
    let failed = errors.iter().map(|e| e.index).collect::<BTreeSet<_>>().len();
    let mut parts =
        vec![format!("REJECTED: {failed} of {total} edit(s) failed validation; NOTHING was applied to {path}.")];
    parts.extend(errors.iter().map(|e| format!("- edits[{}] {}: {}", e.index, e.code, e.message)));
    let mut centers: Vec<usize> = vec![];
    for c in errors.iter().filter_map(|e| e.window_line) {
        if !centers.contains(&c) {
            centers.push(c);
        }
    }
    if !centers.is_empty() && !map.is_empty() {
        parts.push(format!("current content of {path} around the failure(s):"));
        parts.push(windows(map, &centers, radius));
    }
    parts.push(
        "Fix the failing edits using the fresh anchors above and resend the WHOLE batch. Anchors come only from the latest read/edit output for this file — if the target is not visible above, read the file again.".into(),
    );
    parts.join("\n")
}

#[cfg(test)]
mod tests {
    use super::super::anchors::map;
    use super::*;

    fn lines(n: usize) -> Vec<String> {
        (1..=n).map(|i| format!("line-{i}")).collect()
    }

    fn m(lines: &[String]) -> AnchorMap {
        map(&lines.iter().map(String::as_str).collect::<Vec<_>>())
    }

    #[test]
    fn windows_merge_touching_spans_and_separate_distant_ones() {
        let map = m(&lines(20));
        let out = windows(&map, &[5, 7, 18], 2);
        let rows: Vec<&str> = out.lines().collect();
        assert_eq!(rows.iter().filter(|r| **r == "…").count(), 1);
        assert!(rows[0].starts_with(&format!("3:{}|line-3", map.hashes[2])));
        assert!(rows.last().unwrap().starts_with(&format!("20:{}|line-20", map.hashes[19])));
        assert!(!out.contains("line-12"), "the gap is elided");
    }

    #[test]
    fn windows_clamp_at_the_edges() {
        let map = m(&lines(20));
        let out = windows(&map, &[1], 3);
        assert!(out.starts_with(&format!("1:{}|line-1", map.hashes[0])));
        assert_eq!(out.lines().count(), 4);
    }

    #[test]
    fn rejection_is_atomic_per_op_and_windowed() {
        let map = m(&lines(20));
        let errors = [
            OpError {
                index: 1,
                code: "ANCHOR_MISMATCH",
                message: "line 5 does not match hash \"zz\"".into(),
                window_line: Some(5),
            },
            OpError { index: 2, code: "BAD_OP", message: "unknown op \"zap\"".into(), window_line: None },
        ];
        let out = rejection("src/x.ts", 3, &errors, &map, 2);
        assert!(out.starts_with("REJECTED: 2 of 3 edit(s)"), "{out}");
        assert!(out.contains("NOTHING was applied"));
        assert!(out.contains("edits[1] ANCHOR_MISMATCH") && out.contains("edits[2] BAD_OP"));
        assert!(out.contains(&format!("5:{}|line-5", map.hashes[4])), "fresh anchors near the failure");
        assert!(out.contains("resend the WHOLE batch"));
    }

    #[test]
    fn success_summarizes_notes_the_shift_and_refreshes_anchors() {
        let mut new = lines(20);
        new.insert(4, "NEW".into());
        let new = m(&new);
        let region = Region { index: 0, kind: Kind::InsertAfter, old_start: 5, old_end: 4, new_start: 5, new_end: 5 };
        let out = success("src/x.ts", &[region], &new, 20, 1, 1);
        assert!(out.starts_with("ok: applied 1 edit(s)"));
        assert!(out.contains("now 21 lines (was 20)"));
        assert!(out.contains(&format!("5:{}|NEW", new.hashes[4])));
        assert!(out.contains(&format!("4:{}|line-4", new.hashes[3])), "context above");
        assert!(out.contains(&format!("6:{}|line-5", new.hashes[5])), "context below, shifted");
    }

    #[test]
    fn success_for_a_pure_delete_shows_the_seam() {
        let mut new = lines(20);
        new.remove(9);
        let new = m(&new);
        let region = Region { index: 0, kind: Kind::Delete, old_start: 10, old_end: 10, new_start: 10, new_end: 9 };
        let out = success("src/x.ts", &[region], &new, 20, -1, 1);
        assert!(out.contains("deleted line 10"));
        assert!(out.contains(&format!("9:{}|line-9", new.hashes[8])));
        assert!(out.contains(&format!("10:{}|line-11", new.hashes[9])), "below the seam, renumbered");
    }
}
