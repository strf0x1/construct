//! Apply a validated batch to a line array. Ops are applied bottom-up so a
//! splice never shifts a pending op; every coordinate refers to the original
//! snapshot.

use super::ops::{Kind, Resolved};
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Region {
    /// Index into the call's `edits`.
    pub index: usize,
    pub kind: Kind,
    /// Original coordinates the op targeted.
    pub old_start: usize,
    pub old_end: usize,
    /// Where the op's lines landed (1-based, inclusive). For a pure delete,
    /// new_start is the line now at the seam and new_end = new_start - 1.
    pub new_start: usize,
    pub new_end: usize,
}

pub struct Outcome {
    pub lines: Vec<String>,
    pub regions: Vec<Region>,
    /// Net change in line count.
    pub line_delta: isize,
}

pub fn apply(lines: &[String], resolved: &[Resolved]) -> Outcome {
    let mut out = lines.to_vec();

    // Bottom-up; same-gap inserts in reverse edits order, so the file keeps
    // edits order top to bottom.
    let mut descending: Vec<&Resolved> = resolved.iter().collect();
    descending.sort_by(|a, b| b.pos().cmp(&a.pos()).then(b.index.cmp(&a.index)));
    for op in descending {
        out.splice(op.start - 1..op.end, op.insert.iter().cloned());
    }

    // Where each op landed: top-down, accumulating the shift from ops above.
    let mut ascending: Vec<&Resolved> = resolved.iter().collect();
    ascending.sort_by(|a, b| a.pos().cmp(&b.pos()).then(a.index.cmp(&b.index)));
    let mut shift: isize = 0;
    let mut regions = Vec::with_capacity(resolved.len());
    for op in ascending {
        let new_start = (op.start as isize + shift) as usize;
        regions.push(Region {
            index: op.index,
            kind: op.kind,
            old_start: op.start,
            old_end: op.end,
            new_start,
            new_end: new_start + op.insert.len() - 1,
        });
        shift += op.insert.len() as isize - (op.end as isize - op.start as isize + 1);
    }

    let line_delta = out.len() as isize - lines.len() as isize;
    Outcome { lines: out, regions, line_delta }
}

#[cfg(test)]
mod tests {
    use super::super::anchors::map;
    use super::super::ops::validate;
    use super::*;
    use serde_json::{Value, json};

    const LINES: [&str; 10] = ["L1", "L2", "L3", "L4", "L5", "L6", "L7", "L8", "L9", "L10"];

    fn run(ops: impl Fn(&dyn Fn(usize) -> String) -> Value) -> Outcome {
        let m = map(&LINES);
        let a = |n: usize| m.anchor(n);
        let ops = ops(&a);
        let (resolved, errors) = validate(&m, ops.as_array().unwrap(), 256, true);
        assert_eq!(errors, []);
        apply(&m.lines, &resolved)
    }

    #[test]
    fn a_mixed_batch_applies_on_original_coordinates() {
        let o = run(|a| {
            json!([
                { "op": "set", "anchor": a(2), "text": "TWO" },
                { "op": "insert_after", "anchor": a(7), "text": "NEW-A\nNEW-B" },
                { "op": "delete", "anchor": a(5) },
                { "op": "replace_range", "start": a(9), "end": a(10), "text": "TAIL" },
            ])
        });
        assert_eq!(o.lines, ["L1", "TWO", "L3", "L4", "L6", "L7", "NEW-A", "NEW-B", "L8", "TAIL"]);
        assert_eq!(o.line_delta, 0);
    }

    #[test]
    fn batch_order_does_not_matter() {
        let forward =
            run(|a| json!([{ "op": "set", "anchor": a(2), "text": "X" }, { "op": "delete", "anchor": a(8) }]));
        let backward =
            run(|a| json!([{ "op": "delete", "anchor": a(8) }, { "op": "set", "anchor": a(2), "text": "X" }]));
        assert_eq!(forward.lines, backward.lines);
    }

    #[test]
    fn same_gap_inserts_land_in_edits_order() {
        let o = run(|a| {
            json!([
                { "op": "insert_after", "anchor": a(3), "text": "first" },
                { "op": "insert_after", "anchor": a(3), "text": "second" },
                { "op": "insert_before", "anchor": a(4), "text": "third" },
            ])
        });
        assert_eq!(o.lines[2..7], ["L3", "first", "second", "third", "L4"]);
    }

    #[test]
    fn regions_report_new_coordinates() {
        let o = run(|a| {
            json!([
                { "op": "insert_after", "anchor": a(1), "text": "A\nB" },
                { "op": "set", "anchor": a(4), "text": "FOUR" },
                { "op": "delete", "anchor": a(6) },
            ])
        });
        let r = |i: usize| o.regions.iter().find(|r| r.index == i).unwrap();
        assert_eq!((r(0).new_start, r(0).new_end), (2, 3));
        assert_eq!((r(1).new_start, r(1).new_end), (6, 6));
        assert_eq!(r(2).new_end, r(2).new_start - 1, "a pure delete is an empty region at the seam");
        assert_eq!(o.line_delta, 1);
    }

    #[test]
    fn deleting_everything_leaves_no_lines() {
        let m = map(&["only"]);
        let (resolved, errors) = validate(&m, &[json!({ "op": "delete", "anchor": m.anchor(1) })], 256, true);
        assert_eq!(errors, []);
        let o = apply(&m.lines, &resolved);
        assert!(o.lines.is_empty());
        assert_eq!(o.line_delta, -1);
    }
}
