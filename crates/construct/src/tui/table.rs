//! A markdown table drawn as aligned columns. A cell too long for its column
//! wraps onto more lines (a row is as tall as its tallest cell) instead of
//! being cut with `…`, so nothing the model wrote is hidden; the design choice
//! is that only column widths shrink, widest first, never the text.

use super::theme;
use crate::text::cols_after;
use pulldown_cmark::Alignment;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

#[derive(Default)]
pub(super) struct Table {
    pub align: Vec<Alignment>,
    pub rows: Vec<Vec<String>>,
    pub cell: String,
    pub row: Vec<String>,
}

/// Narrowest a column is squeezed to: enough to read a few characters, and
/// still fits a dozen columns in 80.
const MIN_COL: usize = 3;
const SEP: usize = 3;

/// `t` as lines at most `width` columns wide (more columns than fit overflow
/// at `MIN_COL` each, as they always did), ending in a blank line.
pub(super) fn render(t: &Table, width: usize) -> Vec<Line<'static>> {
    let cols = t.rows.iter().map(Vec::len).max().unwrap_or(0);
    if cols == 0 {
        return Vec::new();
    }
    let mut widths = vec![0usize; cols];
    for r in &t.rows {
        for (i, cell) in r.iter().enumerate() {
            widths[i] = widths[i].max(cell.width());
        }
    }
    while widths.iter().sum::<usize>() + SEP * (cols - 1) > width && widths.iter().any(|w| *w > MIN_COL) {
        let Some(i) = (0..cols).max_by_key(|i| widths[*i]) else { break };
        widths[i] -= 1;
    }
    let cells: Vec<Vec<Vec<String>>> = t
        .rows
        .iter()
        .map(|r| (0..cols).map(|i| wrap_cell(r.get(i).map_or("", String::as_str), widths[i])).collect())
        .collect();
    // Wrapped rows run together without a rule between them.
    let ruled = cells.iter().any(|r| r.iter().any(|c| c.len() > 1));
    let rule =
        Line::from(Span::styled(widths.iter().map(|w| "─".repeat(*w)).collect::<Vec<_>>().join("─┼─"), theme::faint()));
    let mut out = Vec::new();
    for (ri, r) in cells.iter().enumerate() {
        let height = r.iter().map(Vec::len).max().unwrap_or(1);
        for li in 0..height {
            let mut spans = Vec::new();
            for (i, &w) in widths.iter().enumerate() {
                let text = r[i].get(li).map_or("", String::as_str);
                let st = if ri == 0 { theme::bold().fg(theme::ACCENT) } else { Style::default() };
                spans.push(Span::styled(pad(text, w, t.align.get(i)), st));
                if i + 1 < cols {
                    spans.push(Span::styled(" │ ", theme::faint()));
                }
            }
            out.push(Line::from(spans));
        }
        if ri == 0 || (ruled && ri + 1 < cells.len()) {
            out.push(rule.clone());
        }
    }
    out.push(Line::default());
    out
}

fn pad(s: &str, w: usize, align: Option<&Alignment>) -> String {
    let pad = w.saturating_sub(s.width());
    match align {
        Some(Alignment::Right) => format!("{}{s}", " ".repeat(pad)),
        Some(Alignment::Center) => format!("{}{s}{}", " ".repeat(pad / 2), " ".repeat(pad - pad / 2)),
        _ => format!("{s}{}", " ".repeat(pad)),
    }
}

/// `s` broken at spaces into lines of at most `w` columns; a word wider than
/// `w` is broken mid-word. Always at least one line.
fn wrap_cell(s: &str, w: usize) -> Vec<String> {
    let w = w.max(1);
    let mut lines = Vec::new();
    let mut cur = String::new();
    let mut cur_w = 0;
    for word in s.split_whitespace() {
        let ww = word.width();
        if cur_w > 0 && cur_w + 1 + ww <= w {
            cur.push(' ');
            cur.push_str(word);
            cur_w += 1 + ww;
            continue;
        }
        if cur_w > 0 {
            lines.push(std::mem::take(&mut cur));
            cur_w = 0;
        }
        let mut prev = None;
        for c in word.chars() {
            let cw = cols_after(prev, c);
            if cur_w + cw > w && cur_w > 0 {
                lines.push(std::mem::take(&mut cur));
                cur_w = 0;
            }
            cur.push(c);
            cur_w += cw;
            prev = Some(c);
        }
    }
    lines.push(cur);
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::prop;

    fn table(rows: &[&[&str]]) -> Table {
        Table { rows: rows.iter().map(|r| r.iter().map(|c| c.to_string()).collect()).collect(), ..Default::default() }
    }

    fn plain(l: &Line) -> String {
        l.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn a_long_cell_wraps_and_loses_no_words() {
        let t = table(&[&["Aspect", "How"], &["CI", "The cargo xtask check command runs the arch gate in CI"]]);
        let lines: Vec<String> = render(&t, 30).iter().map(plain).collect();
        assert!(lines.iter().all(|l| !l.contains('…')), "{lines:#?}");
        assert!(lines.iter().all(|l| prop::cols(&Line::raw(l.clone())) <= 30), "{lines:#?}");
        let words: Vec<&str> =
            lines.iter().flat_map(|l| l.split(" │ ").nth(1)).flat_map(str::split_whitespace).collect();
        assert!(words.join(" ").ends_with("The cargo xtask check command runs the arch gate in CI"), "{words:?}");
    }

    #[test]
    fn a_word_wider_than_the_column_breaks_mid_word() {
        assert_eq!(wrap_cell("abcdefgh", 3), ["abc", "def", "gh"]);
        assert_eq!(wrap_cell("", 3), [""]);
    }

    #[test]
    fn a_table_that_fits_is_unchanged() {
        let lines: Vec<String> = render(&table(&[&["a", "b"], &["1", "2"]]), 40).iter().map(plain).collect();
        assert_eq!(lines, ["a │ b", "──┼──", "1 │ 2", ""]);
    }
}
