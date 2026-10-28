//! Rounded, titled boxes of aligned rows, drawn into plain transcript lines:
//! the one look `/help`, `/status`, `/memory`, `/models` and `/skills` share.
//! A box is data (`Section`); width, columns, wrapping and colors are derived,
//! so a command that lists things builds rows and never draws a border.

use super::theme;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// One table row: name, argument (or alternate keys), description.
pub(super) type Row = (String, &'static str, String);

/// A box: `title` in the top edge, `rows` inside. `name` colors the first
/// column; a row's argument is always yellow, so it reads as a different kind
/// of thing from the name.
pub(super) struct Section {
    pub(super) title: String,
    pub(super) border: Color,
    pub(super) name: Color,
    pub(super) rows: Vec<Row>,
}

impl Section {
    pub(super) fn new(title: impl Into<String>, color: Color, rows: Vec<Row>) -> Self {
        Self { title: title.into(), border: color, name: color, rows }
    }
}

/// Narrow and wide ceilings for a box. Beyond one, a wide terminal only
/// spreads the descriptions out; below `MIN_BOX` the columns have no room to wrap.
pub(super) const NARROW: usize = 78;
pub(super) const WIDE: usize = 100;
const MIN_BOX: usize = 40;

pub(super) fn box_width(width: usize, max: usize) -> usize {
    width.saturating_sub(2).clamp(MIN_BOX, max)
}

/// The boxes for a transcript `width` (at most `max` wide), indented to line
/// up with notices, then `note` as dim text underneath.
pub(super) fn lines(width: usize, max: usize, sections: &[Section], note: &str) -> Vec<Line<'static>> {
    let box_w = box_width(width, max);
    let mut out: Vec<Line<'static>> = sections.iter().flat_map(|s| panel(s, box_w)).collect();
    for l in note.lines().flat_map(|l| words(l, box_w.saturating_sub(2))) {
        out.push(Line::from(Span::styled(format!("   {l}"), theme::dim())));
    }
    out
}

fn panel(s: &Section, box_w: usize) -> Vec<Line<'static>> {
    let edge = Style::new().fg(s.border);
    let inner = box_w - 2; // between the corners
    let label_w = s.rows.iter().map(label_width).max().unwrap_or(0);
    // padding 1 each side, 2 between the columns
    let about_w = inner.saturating_sub(label_w + 4).max(10);

    let rest = inner.saturating_sub(s.title.width() + 3);
    let mut out = vec![Line::from(vec![
        Span::raw("  "),
        Span::styled("╭─ ", edge),
        Span::styled(s.title.clone(), edge.add_modifier(Modifier::BOLD)),
        Span::styled(format!(" {}╮", "─".repeat(rest)), edge),
    ])];
    for e in &s.rows {
        let about = e.2.replace('\t', "  ");
        for (i, text) in about.split('\n').flat_map(|l| words(l, about_w)).enumerate() {
            let mut row = vec![Span::raw("  "), Span::styled("│ ", edge)];
            let used = if i == 0 {
                row.push(Span::styled(e.0.clone(), Style::new().fg(s.name).add_modifier(Modifier::BOLD)));
                if !e.1.is_empty() {
                    row.push(Span::styled(format!(" {}", e.1), Style::new().fg(theme::YELLOW)));
                }
                label_width(e)
            } else {
                0
            };
            row.push(Span::raw(" ".repeat(label_w - used + 2)));
            let text_w = text.width();
            row.push(Span::raw(text));
            let tail = inner.saturating_sub(label_w + 4 + text_w);
            row.push(Span::styled(format!("{} │", " ".repeat(tail)), edge));
            out.push(Line::from(row));
        }
    }
    out.push(Line::from(vec![Span::raw("  "), Span::styled(format!("╰{}╯", "─".repeat(inner)), edge)]));
    out
}

fn label_width(e: &Row) -> usize {
    e.0.width() + if e.1.is_empty() { 0 } else { 1 + e.1.width() }
}

/// Greedy word wrap to `w` columns. Runs of spaces survive, so a row can
/// carry its own aligned columns; an empty text is one empty line.
fn words(text: &str, w: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut cur = String::new();
    for (i, word) in text.split(' ').enumerate() {
        if i > 0 {
            if !word.is_empty() && !cur.trim().is_empty() && cur.width() + 1 + word.width() > w {
                lines.push(cur.trim_end().to_string());
                cur = word.to_string();
                continue;
            }
            cur.push(' ');
        }
        cur.push_str(word);
    }
    lines.push(cur.trim_end().to_string());
    lines.into_iter().flat_map(|l| chunks(l, w)).collect()
}

/// `line` cut into pieces of at most `w` columns, for a word (a path) that
/// has no space to wrap at.
fn chunks(line: String, w: usize) -> Vec<String> {
    if line.width() <= w {
        return vec![line];
    }
    let mut out = vec![String::new()];
    for c in line.chars() {
        let last = out.last_mut().unwrap_or_else(|| unreachable!("seeded with one"));
        if last.width() + c.width().unwrap_or(0) > w {
            out.push(c.to_string());
        } else {
            last.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn width_of(l: &Line) -> usize {
        l.spans.iter().map(|s| s.content.width()).sum()
    }

    fn section() -> Section {
        let long = "word ".repeat(40);
        Section::new(
            "demo",
            theme::ACCENT,
            vec![
                ("a".into(), "", "short".into()),
                ("longer-name".into(), "[arg]", long),
                ("b".into(), "", "x\ny".into()),
            ],
        )
    }

    #[test]
    fn boxes_are_rectangles_at_any_width() {
        for w in [42usize, 60, 80, 200] {
            for max in [NARROW, WIDE] {
                for l in lines(w, max, &[section()], "") {
                    assert_eq!(width_of(&l), box_width(w, max) + 2, "ragged row at width {w}: {l:?}");
                }
            }
        }
    }

    #[test]
    fn newlines_start_a_new_row_and_spaces_keep_their_columns() {
        let s = Section::new(
            "t",
            theme::ACCENT,
            vec![("id".into(), "", "  32k  $1/$2".into()), ("z".into(), "", "a\nb".into())],
        );
        let text: Vec<String> = lines(80, NARROW, &[s], "")
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.to_string()).collect())
            .collect();
        assert!(text[1].contains("id      32k  $1/$2"), "{text:?}");
        assert!(text[2].contains("│ z  a") && text[3].contains("│    b"), "{text:?}");
    }

    #[test]
    fn a_word_longer_than_the_box_is_cut_to_fit() {
        let s = Section::new("t", theme::ACCENT, vec![("path".into(), "", format!("/{}", "x".repeat(150)))]);
        for l in lines(60, NARROW, &[s], "") {
            assert_eq!(width_of(&l), 60 - 2 + 2, "{l:?}");
        }
    }

    #[test]
    fn a_note_goes_under_the_boxes() {
        let out = lines(80, NARROW, &[section()], "first\nsecond");
        let last: String = out.last().unwrap().spans.iter().map(|s| s.content.to_string()).collect();
        assert_eq!(last.trim(), "second");
    }
}
