//! Exact-text replacement with opencode's forgiving tiers: an exact match
//! first, then one that ignores typographic quote/dash/space variants, then a
//! line-wise match that ignores trailing whitespace. CRLF files are matched in
//! their LF view and keep their endings.

use crate::tools::ToolError;

#[derive(Clone, Copy)]
struct Span {
    start: usize,
    end: usize,
}

fn normalize_char(c: char) -> char {
    match c {
        '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' => '\'',
        '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' => '"',
        '\u{2010}'..='\u{2015}' | '\u{2212}' => '-',
        '\u{00A0}' | '\u{2002}'..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}' => ' ',
        c => c,
    }
}

/// Normalize while keeping byte offsets valid: every replacement above maps
/// to an ASCII char, so we record where each normalized byte came from.
fn normalized_with_map(s: &str) -> (String, Vec<usize>) {
    let mut out = String::with_capacity(s.len());
    let mut map = Vec::with_capacity(s.len() + 1);
    for (i, c) in s.char_indices() {
        let n = normalize_char(c);
        for _ in 0..n.len_utf8() {
            map.push(i);
        }
        out.push(n);
    }
    map.push(s.len());
    (out, map)
}

fn find_all(hay: &str, needle: &str) -> Vec<Span> {
    hay.match_indices(needle).map(|(start, m)| Span { start, end: start + m.len() }).collect()
}

fn find_lines(hay: &str, needle: &str) -> Vec<Span> {
    let trailing_nl = needle.ends_with('\n');
    let mut want: Vec<&str> = needle.split('\n').collect();
    if trailing_nl {
        want.pop();
    }
    let key = |s: &str| s.trim_end().chars().map(normalize_char).collect::<String>();
    let want: Vec<String> = want.iter().map(|l| key(l)).collect();
    // (start, end_without_newline, end_with_newline)
    let mut lines = Vec::new();
    let mut pos = 0;
    for l in hay.split_inclusive('\n') {
        let body = l.strip_suffix('\n').unwrap_or(l);
        lines.push((pos, pos + body.len(), pos + l.len(), body));
        pos += l.len();
    }
    let mut out: Vec<Span> = Vec::new();
    if want.is_empty() {
        return out;
    }
    for i in 0..lines.len() {
        if i + want.len() > lines.len() {
            break;
        }
        if (0..want.len()).all(|k| key(lines[i + k].3) == want[k]) {
            let last = lines[i + want.len() - 1];
            let end = if trailing_nl { last.2 } else { last.1 };
            let span = Span { start: lines[i].0, end };
            if !out.iter().any(|m| m.end > span.start && m.start < span.end) {
                out.push(span);
            }
        }
    }
    out
}

fn locate(view: &str, search: &str) -> Vec<Span> {
    let exact = find_all(view, search);
    if !exact.is_empty() {
        return exact;
    }
    let (nview, map) = normalized_with_map(view);
    let (nsearch, _) = normalized_with_map(search);
    let fuzzy: Vec<Span> =
        find_all(&nview, &nsearch).into_iter().map(|s| Span { start: map[s.start], end: map[s.end] }).collect();
    if !fuzzy.is_empty() {
        return fuzzy;
    }
    find_lines(view, search)
}

/// Replace `old` with `new` in `text`. Unless `all`, `old` must match exactly
/// once. Error messages are written for the model to act on; codes are
/// `no_change`, `not_found` and `ambiguous`.
pub fn replace_once(text: &str, old: &str, new: &str, all: bool) -> Result<String, ToolError> {
    if old.is_empty() {
        return Err(ToolError::new("no_change", "old_string is empty; use write to create or overwrite a file"));
    }
    if old == new {
        return Err(ToolError::new("no_change", "old_string and new_string are identical; nothing to change"));
    }
    let crlf = text.contains("\r\n");
    let view = if crlf { text.replace("\r\n", "\n") } else { text.to_string() };
    let search = old.replace("\r\n", "\n");
    let replacement = new.replace("\r\n", "\n");
    let spans = locate(&view, &search);
    match spans.len() {
        0 => {
            return Err(ToolError::new(
                "not_found",
                "old_string not found. It must match the file exactly, including indentation; re-read the file and copy the text",
            ));
        }
        1 => {}
        n if !all => {
            return Err(ToolError::new(
                "ambiguous",
                format!("old_string matches {n} places; add surrounding lines to make it unique, or set replace_all"),
            ));
        }
        _ => {}
    }
    let mut out = view.clone();
    for s in spans.iter().rev() {
        out.replace_range(s.start..s.end, &replacement);
    }
    Ok(if crlf { out.replace('\n', "\r\n") } else { out })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::prop;
    use proptest::prelude::*;

    const CODE: &[&str] = &["\n", "\r\n", "    ", "{", "}", "\u{2019}", "\u{201c}", "\u{2014}", "\u{a0}", "fn ", "x", " \n"];

    proptest! {
        #![proptest_config(prop::config())]

        #[test]
        fn replace_never_panics(
            text in prop::text_with(CODE),
            old in prop::text_with(CODE),
            new in prop::text_with(CODE),
            all in any::<bool>(),
        ) {
            let _ = replace_once(&text, &old, &new, all);
        }

        /// The tool's promise: an `old` found once is swapped for `new`,
        /// and the rest of the file is left byte for byte.
        #[test]
        fn a_unique_match_is_replaced_and_nothing_else_moves(
            before in prop::text_with(CODE),
            old in prop::text_with(CODE),
            after in prop::text_with(CODE),
            new in prop::text_with(CODE),
        ) {
            // CRLF files are rewritten in full; their promise is the e2e tests'.
            let [before, old, after, new] = [before, old, after, new].map(|s| s.replace('\r', ""));
            let text = format!("{before}{old}{after}");
            prop_assume!(!old.is_empty() && old != new && text.matches(old.as_str()).count() == 1);
            let out = replace_once(&text, &old, &new, false).unwrap();
            prop_assert_eq!(out, format!("{before}{new}{after}"));
        }
    }
}
