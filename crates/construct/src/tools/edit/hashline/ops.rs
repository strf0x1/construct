//! Edit operations: shape checks, anchor resolution and overlap checks. The
//! whole batch is checked before anything is applied, every error is
//! collected, and any error rejects the batch.

use super::anchors::{self, AnchorMap};
use serde::Serialize;
use serde_json::Value;

pub const OP_NAMES: [&str; 6] = ["set", "replace_range", "insert_after", "insert_before", "delete", "delete_range"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Set,
    ReplaceRange,
    InsertAfter,
    InsertBefore,
    Delete,
    DeleteRange,
}

impl Kind {
    fn parse(s: &str) -> Option<Kind> {
        Some(match s {
            "set" => Kind::Set,
            "replace_range" => Kind::ReplaceRange,
            "insert_after" => Kind::InsertAfter,
            "insert_before" => Kind::InsertBefore,
            "delete" => Kind::Delete,
            "delete_range" => Kind::DeleteRange,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone)]
pub struct Resolved {
    /// Index into the call's `edits`.
    pub index: usize,
    pub kind: Kind,
    /// First replaced line (1-based). For inserts, the gap + 1 (an empty range).
    pub start: usize,
    /// Last replaced line. For inserts, the gap (start - 1).
    pub end: usize,
    /// Replacement lines; empty for deletes.
    pub insert: Vec<String>,
    /// An echoed anchor prefix was stripped from `text`.
    pub echo_stripped: bool,
}

impl Resolved {
    /// Sort position in half-lines: content ops at their start line, inserts
    /// in the gap they target, so applying bottom-up never shifts a pending op.
    pub fn pos(&self) -> usize {
        if self.is_insert() { 2 * self.end + 1 } else { 2 * self.start }
    }
    pub fn is_insert(&self) -> bool {
        self.end < self.start
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OpError {
    pub index: usize,
    pub code: &'static str,
    pub message: String,
    /// The line whose neighbourhood helps recovery, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_line: Option<usize>,
}

impl OpError {
    fn new(index: usize, code: &'static str, message: String) -> Self {
        Self { index, code, message, window_line: None }
    }
}

/// An op's `text` as lines: CRs dropped, one trailing newline forgiven (it
/// separates, it isn't an extra blank line).
pub fn text_to_lines(text: &str) -> Vec<String> {
    let t = text.replace('\r', "");
    let t = t.strip_suffix('\n').unwrap_or(&t);
    t.split('\n').map(str::to_string).collect()
}

/// `LINE:HASH|` at the start of a line, as read output shows it.
fn echo_prefix_len(line: &str) -> Option<usize> {
    let trimmed = line.trim_start();
    let lead = line.len() - trimmed.len();
    let digits = trimmed.bytes().take_while(u8::is_ascii_digit).count();
    let rest = trimmed[digits..].strip_prefix(':').filter(|_| digits > 0)?;
    let letters = rest.bytes().take_while(u8::is_ascii_lowercase).count();
    if !(2..=13).contains(&letters) || rest.as_bytes().get(letters) != Some(&b'|') {
        return None;
    }
    Some(lead + digits + 1 + letters + 1)
}

/// Models paste read output back as payload. When every non-blank line has
/// a `LINE:HASH|` prefix, strip them; a partial match is left alone, since
/// content may legitimately look like one.
pub fn strip_echoed_anchors(lines: Vec<String>) -> Vec<String> {
    let mut non_blank = lines.iter().filter(|l| !l.trim().is_empty()).peekable();
    if non_blank.peek().is_none() || !non_blank.all(|l| echo_prefix_len(l).is_some()) {
        return lines;
    }
    lines
        .into_iter()
        .map(|l| match echo_prefix_len(&l) {
            Some(n) => l[n..].to_string(),
            None => l,
        })
        .collect()
}

/// Echo forgiveness, two exact triggers, both reported and both switched off
/// by `strip_echoes = false` (for repos whose files hold `LINE:HASH|` lines,
/// like docs about this tool).
///
/// (a) every non-blank line has a `LINE:HASH|` prefix: strip them.
/// (b) the first line starts with the op's own validated anchor, `3:ab|` or
///     `ab|` (seen in the wild: `"ab|GAMMA"` for an op anchored at `3:ab`).
fn forgive_echoes(insert: Vec<String>, target: Option<(usize, &str)>, enabled: bool) -> (Vec<String>, bool) {
    if !enabled || insert.is_empty() {
        return (insert, false);
    }
    let original = insert.clone();
    let mut out = strip_echoed_anchors(insert);
    if let Some((line, hash)) = target {
        for prefix in [format!("{line}:{hash}|"), format!("{hash}|")] {
            if let Some(rest) = out[0].strip_prefix(&prefix) {
                out[0] = rest.to_string();
                break;
            }
        }
    }
    let stripped = out != original;
    (out, stripped)
}

fn anchor_field(
    map: &AnchorMap,
    index: usize,
    field: &str,
    value: Option<&Value>,
    errors: &mut Vec<OpError>,
) -> Option<usize> {
    let Some(raw) = value.and_then(Value::as_str).filter(|s| !s.trim().is_empty()) else {
        errors.push(OpError::new(
            index,
            "MISSING_FIELD",
            format!("\"{field}\" is required and must be a \"LINE:HASH\" anchor string copied from the latest read/edit output"),
        ));
        return None;
    };
    let Some(a) = anchors::parse(raw) else {
        errors.push(OpError::new(
            index,
            "BAD_ANCHOR",
            format!("\"{field}\": \"{raw}\" is not a valid anchor; expected \"LINE:HASH\" like \"12:ax\", copied exactly from the latest read/edit output"),
        ));
        return None;
    };
    if let Err((failure, message)) = anchors::check(map, &a) {
        errors.push(OpError {
            index,
            code: failure.code(),
            message: format!("\"{field}\": {message}"),
            window_line: Some(a.line.min(map.len().max(1))),
        });
        return None;
    }
    Some(a.line)
}

fn require_text(op: &Value, kind: &str, index: usize, errors: &mut Vec<OpError>) -> Option<Vec<String>> {
    match op.get("text").and_then(Value::as_str) {
        Some(t) => Some(text_to_lines(t)),
        None => {
            errors.push(OpError::new(
                index,
                "MISSING_FIELD",
                format!("\"text\" is required for op \"{kind}\" (use \"\" for a blank line)"),
            ));
            None
        }
    }
}

pub fn validate(map: &AnchorMap, ops: &[Value], max_ops: usize, strip_echoes: bool) -> (Vec<Resolved>, Vec<OpError>) {
    let mut resolved = Vec::new();
    let mut errors = Vec::new();
    if ops.len() > max_ops {
        errors.push(OpError::new(0, "BAD_OP", format!("too many edits in one call ({} > {max_ops})", ops.len())));
        return (resolved, errors);
    }

    for (index, op) in ops.iter().enumerate() {
        let name = op.get("op").and_then(Value::as_str);
        let Some(kind) = name.and_then(Kind::parse) else {
            let shown = match op.get("op") {
                Some(Value::String(s)) => s.clone(),
                Some(v) => v.to_string(),
                None => "undefined".into(),
            };
            errors.push(OpError::new(
                index,
                "BAD_OP",
                format!("unknown op \"{shown}\"; valid ops: {}", OP_NAMES.join(", ")),
            ));
            continue;
        };
        let name = name.unwrap_or_default();
        let hash = |line: usize| map.hashes[line - 1].as_str();
        let r =
            match kind {
                Kind::Set => {
                    let line = anchor_field(map, index, "anchor", op.get("anchor"), &mut errors);
                    let insert = require_text(op, name, index, &mut errors);
                    line.zip(insert).map(|(line, insert)| {
                        let (insert, echo_stripped) = forgive_echoes(insert, Some((line, hash(line))), strip_echoes);
                        Resolved { index, kind, start: line, end: line, insert, echo_stripped }
                    })
                }
                Kind::ReplaceRange | Kind::DeleteRange => {
                    let start = anchor_field(map, index, "start", op.get("start"), &mut errors);
                    let end = anchor_field(map, index, "end", op.get("end"), &mut errors);
                    let insert = if kind == Kind::ReplaceRange {
                        require_text(op, name, index, &mut errors)
                    } else {
                        Some(vec![])
                    };
                    match (start, end, insert) {
                        (Some(start), Some(end), Some(_)) if end < start => {
                            errors.push(OpError {
                                index,
                                code: "BAD_RANGE",
                                message: format!("range end (line {end}) is before range start (line {start})"),
                                window_line: Some(start),
                            });
                            None
                        }
                        (Some(start), Some(end), Some(insert)) => {
                            let target = (kind == Kind::ReplaceRange).then(|| (start, hash(start)));
                            let (insert, echo_stripped) = forgive_echoes(insert, target, strip_echoes);
                            Some(Resolved { index, kind, start, end, insert, echo_stripped })
                        }
                        _ => None,
                    }
                }
                Kind::InsertAfter | Kind::InsertBefore => {
                    let line = anchor_field(map, index, "anchor", op.get("anchor"), &mut errors);
                    let insert = require_text(op, name, index, &mut errors);
                    line.zip(insert).map(|(line, insert)| {
                        let gap = if kind == Kind::InsertAfter { line } else { line - 1 };
                        let (insert, echo_stripped) = forgive_echoes(insert, None, strip_echoes);
                        Resolved { index, kind, start: gap + 1, end: gap, insert, echo_stripped }
                    })
                }
                Kind::Delete => anchor_field(map, index, "anchor", op.get("anchor"), &mut errors)
                    .map(|line| Resolved { index, kind, start: line, end: line, insert: vec![], echo_stripped: false }),
            };
        resolved.extend(r);
    }

    find_overlaps(&resolved, &mut errors);
    (resolved, errors)
}

/// Reject ops that step on each other: two content ops sharing a line, or an
/// insert into a gap strictly inside another op's range. Inserts at a range's
/// outer edges, and several inserts into one gap, are fine.
fn find_overlaps(resolved: &[Resolved], errors: &mut Vec<OpError>) {
    let content: Vec<&Resolved> = resolved.iter().filter(|r| !r.is_insert()).collect();
    for (a, x) in content.iter().enumerate() {
        for y in &content[a + 1..] {
            if x.start <= y.end && y.start <= x.end {
                errors.push(OpError {
                    index: y.index,
                    code: "OVERLAPPING_OPS",
                    message: format!(
                        "edits[{}] (lines {}-{}) overlaps edits[{}] (lines {}-{}); each line may be touched by at most one op",
                        y.index, y.start, y.end, x.index, x.start, x.end
                    ),
                    window_line: Some(y.start),
                });
            }
        }
    }
    for ins in resolved.iter().filter(|r| r.is_insert()) {
        let gap = ins.end;
        for c in &content {
            if c.start <= gap && gap < c.end {
                errors.push(OpError {
                    index: ins.index,
                    code: "OVERLAPPING_OPS",
                    message: format!(
                        "edits[{}] inserts inside the range replaced by edits[{}] (lines {}-{}); insert outside it or fold the text into that op",
                        ins.index, c.index, c.start, c.end
                    ),
                    window_line: Some(c.start),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::anchors::map;
    use super::*;
    use serde_json::json;

    const LINES: [&str; 8] = ["one", "two", "three", "four", "five", "six", "seven", "eight"];

    fn setup() -> (AnchorMap, impl Fn(usize) -> String) {
        let m = map(&LINES);
        let hashes = m.hashes.clone();
        (m, move |n: usize| format!("{n}:{}", hashes[n - 1]))
    }

    fn ok(m: &AnchorMap, ops: Value) -> Vec<Resolved> {
        let (r, errors) = validate(m, ops.as_array().unwrap(), 256, true);
        assert_eq!(errors, []);
        r
    }

    fn codes(m: &AnchorMap, ops: Value) -> Vec<(usize, &'static str)> {
        validate(m, ops.as_array().unwrap(), 256, true).1.iter().map(|e| (e.index, e.code)).collect()
    }

    #[test]
    fn every_op_kind_resolves_with_correct_coordinates() {
        let (m, a) = setup();
        let r = ok(
            &m,
            json!([
                { "op": "set", "anchor": a(2), "text": "TWO" },
                { "op": "replace_range", "start": a(3), "end": a(4), "text": "X\nY\nZ" },
                { "op": "insert_after", "anchor": a(5), "text": "after5" },
                { "op": "insert_before", "anchor": a(6), "text": "before6" },
                { "op": "delete", "anchor": a(7) },
                { "op": "delete_range", "start": a(8), "end": a(8) },
            ]),
        );
        let got: Vec<_> = r.iter().map(|r| (r.kind, r.start, r.end, r.pos(), r.insert.len())).collect();
        assert_eq!(
            got,
            [
                (Kind::Set, 2, 2, 4, 1),
                (Kind::ReplaceRange, 3, 4, 6, 3),
                (Kind::InsertAfter, 6, 5, 11, 1),
                (Kind::InsertBefore, 6, 5, 11, 1),
                (Kind::Delete, 7, 7, 14, 0),
                (Kind::DeleteRange, 8, 8, 16, 0),
            ]
        );
    }

    #[test]
    fn all_errors_are_collected() {
        let (m, a) = setup();
        let got = codes(
            &m,
            json!([
                { "op": "set", "anchor": "1:zz", "text": "x" },
                { "op": "frobnicate" },
                { "op": "set", "anchor": a(2) },
                { "op": "replace_range", "start": a(4), "end": a(3), "text": "x" },
                { "op": "set", "anchor": "not-an-anchor", "text": "x" },
            ]),
        );
        assert_eq!(
            got,
            [(0, "ANCHOR_MISMATCH"), (1, "BAD_OP"), (2, "MISSING_FIELD"), (3, "BAD_RANGE"), (4, "BAD_ANCHOR")]
        );
    }

    #[test]
    fn overlapping_content_ops_are_rejected() {
        let (m, a) = setup();
        let got = codes(
            &m,
            json!([
                { "op": "replace_range", "start": a(2), "end": a(4), "text": "x" },
                { "op": "delete", "anchor": a(3) },
            ]),
        );
        assert_eq!(got, [(1, "OVERLAPPING_OPS")]);
    }

    #[test]
    fn inserts_inside_a_range_are_rejected_edges_are_fine() {
        let (m, a) = setup();
        let inside = codes(
            &m,
            json!([
                { "op": "replace_range", "start": a(2), "end": a(5), "text": "x" },
                { "op": "insert_after", "anchor": a(3), "text": "y" },
            ]),
        );
        assert_eq!(inside, [(1, "OVERLAPPING_OPS")]);
        ok(
            &m,
            json!([
                { "op": "replace_range", "start": a(2), "end": a(5), "text": "x" },
                { "op": "insert_after", "anchor": a(5), "text": "below" },
                { "op": "insert_before", "anchor": a(2), "text": "above" },
            ]),
        );
    }

    #[test]
    fn line_out_of_range_is_reported_as_such() {
        let (m, _) = setup();
        assert_eq!(codes(&m, json!([{ "op": "delete", "anchor": "99:ab" }])), [(0, "LINE_OUT_OF_RANGE")]);
    }

    #[test]
    fn op_count_is_capped() {
        let (m, a) = setup();
        let many: Vec<Value> = (0..10).map(|_| json!({ "op": "delete", "anchor": a(1) })).collect();
        let (_, errors) = validate(&m, &many, 5, true);
        assert_eq!(errors[0].code, "BAD_OP");
        assert!(errors[0].message.contains("too many"));
    }

    #[test]
    fn text_to_lines_forgives_one_trailing_newline() {
        assert_eq!(text_to_lines("a\nb\n"), ["a", "b"]);
        assert_eq!(text_to_lines("a\r\nb"), ["a", "b"]);
        assert_eq!(text_to_lines(""), [""]);
        assert_eq!(text_to_lines("a\n\nb"), ["a", "", "b"]);
        assert_eq!(text_to_lines("a\n\n"), ["a", ""]);
    }

    #[test]
    fn the_targets_own_hash_echoed_into_text_is_stripped() {
        let (m, a) = setup();
        let h = |n: usize| m.hashes[n - 1].clone();
        let one = |op: Value| ok(&m, json!([op])).remove(0);
        let r = one(json!({ "op": "set", "anchor": a(2), "text": format!("{}|TWO", h(2)) }));
        assert_eq!((r.insert, r.echo_stripped), (vec!["TWO".to_string()], true));
        let r = one(json!({ "op": "set", "anchor": a(2), "text": format!("2:{}|TWO", h(2)) }));
        assert_eq!((r.insert, r.echo_stripped), (vec!["TWO".to_string()], true));
        let r = one(json!({ "op": "replace_range", "start": a(3), "end": a(4), "text": format!("{}|X\nY", h(3)) }));
        assert_eq!((r.insert, r.echo_stripped), (vec!["X".to_string(), "Y".to_string()], true));
        // Another line's hash is not the trigger.
        let r = one(json!({ "op": "set", "anchor": a(2), "text": format!("{}|kept", h(5)) }));
        assert_eq!((r.insert, r.echo_stripped), (vec![format!("{}|kept", h(5))], false));
        // Shell pipelines are never touched.
        let r = one(json!({ "op": "set", "anchor": a(2), "text": "ls|wc -l" }));
        assert_eq!((r.insert, r.echo_stripped), (vec!["ls|wc -l".to_string()], false));
    }

    #[test]
    fn strip_echoes_false_disables_both_forms() {
        let (m, a) = setup();
        let h = m.hashes[1].clone();
        for text in [format!("{h}|TWO"), format!("2:{h}|TWO")] {
            let (r, errors) = validate(&m, &[json!({ "op": "set", "anchor": a(2), "text": text })], 256, false);
            assert_eq!(errors, []);
            assert_eq!((r[0].insert.clone(), r[0].echo_stripped), (vec![text], false));
        }
    }

    #[test]
    fn echoes_are_stripped_only_when_every_non_blank_line_has_one() {
        let v = |s: &[&str]| s.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(strip_echoed_anchors(v(&["12:ax|foo", "13:vka|bar"])), ["foo", "bar"]);
        assert_eq!(strip_echoed_anchors(v(&["12:ax|foo", "", "14:zz|baz"])), ["foo", "", "baz"]);
        assert_eq!(strip_echoed_anchors(v(&["12:ax|foo", "plain"])), ["12:ax|foo", "plain"]);
        assert_eq!(strip_echoed_anchors(v(&["plain"])), ["plain"]);
        assert_eq!(strip_echoed_anchors(v(&[""])), [""]);
    }
}
