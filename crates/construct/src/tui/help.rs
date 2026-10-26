//! The `/help` panel: commands and keys as rounded boxes, drawn into plain
//! transcript lines. The tables below are the only place a command or key is
//! described; the box, columns and colors are derived from them, so a new
//! entry needs one line and nothing else.

use super::theme;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

/// One row: what to type, an optional argument shown after it, what it does.
struct Entry {
    name: &'static str,
    arg: &'static str,
    about: &'static str,
}

const fn entry(name: &'static str, arg: &'static str, about: &'static str) -> Entry {
    Entry { name, arg, about }
}

const COMMANDS: &[Entry] = &[
    entry("/new", "", "fresh session (also /clear)"),
    entry("/model", "<id>", "switch model for the next turn"),
    entry("/skills", "", "list the skill library"),
    entry("/memory", "", "show what construct remembers"),
    entry("/learn", "", "run the learning pass now"),
    entry("/status", "", "session id, journal and totals"),
    entry("/copy", "[error|status]", "last reply (or last tool error, or status) to the clipboard"),
    entry("/help", "", "this list (also /?)"),
    entry("/quit", "", "exit (also /exit, /q)"),
];

const KEYS: &[Entry] = &[
    entry("enter", "", "send"),
    entry("alt+enter", "ctrl+j", "newline"),
    entry("esc", "", "interrupt"),
    entry("ctrl+o", "", "expand tool output"),
    entry("wheel", "↑↓ pgup pgdn", "scroll"),
    entry("ctrl+p", "ctrl+n", "input history"),
    entry("ctrl+c", "", "clear the input, then quit"),
];

const NOTE: &str = "select text with the mouse (shift-drag if your terminal asks for it); /copy works over ssh";

/// Widest and narrowest box. Beyond the first, a wide terminal only spreads
/// the descriptions out; below the second the columns have no room to wrap.
const MAX_BOX: usize = 78;
const MIN_BOX: usize = 40;

/// The whole panel for a transcript `width`, indented to line up with notices.
pub(super) fn lines(width: usize) -> Vec<Line<'static>> {
    let box_w = width.saturating_sub(2).clamp(MIN_BOX, MAX_BOX);
    let mut out = panel("commands", theme::ACCENT, COMMANDS, theme::ACCENT, box_w);
    out.extend(panel("keys", theme::ACCENT2, KEYS, theme::CYAN, box_w));
    for l in words(NOTE, box_w.saturating_sub(2)) {
        out.push(Line::from(Span::styled(format!("   {l}"), theme::dim())));
    }
    out
}

/// A rounded box titled `title`, one aligned row per entry. `name` colors the
/// first column; an entry's `arg` (an argument, or alternate keys) is always
/// yellow, so it reads as a different kind of thing from the name.
fn panel(title: &str, border: Color, entries: &[Entry], name: Color, box_w: usize) -> Vec<Line<'static>> {
    let edge = Style::new().fg(border);
    let inner = box_w - 2; // between the corners
    let label_w = entries.iter().map(label_width).max().unwrap_or(0);
    // padding 1 each side, 2 between the columns
    let about_w = inner.saturating_sub(label_w + 4).max(10);

    let rest = inner.saturating_sub(title.width() + 3);
    let mut out = vec![Line::from(vec![
        Span::raw("  "),
        Span::styled("╭─ ", edge),
        Span::styled(title.to_string(), edge.add_modifier(Modifier::BOLD)),
        Span::styled(format!(" {}╮", "─".repeat(rest)), edge),
    ])];
    for e in entries {
        for (i, text) in words(e.about, about_w).into_iter().enumerate() {
            let mut row = vec![Span::raw("  "), Span::styled("│ ", edge)];
            let used = if i == 0 {
                row.push(Span::styled(e.name, Style::new().fg(name).add_modifier(Modifier::BOLD)));
                if !e.arg.is_empty() {
                    row.push(Span::styled(format!(" {}", e.arg), Style::new().fg(theme::YELLOW)));
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

fn label_width(e: &Entry) -> usize {
    e.name.width() + if e.arg.is_empty() { 0 } else { 1 + e.arg.width() }
}

/// Greedy word wrap to `w` columns; an empty text is one empty line.
fn words(text: &str, w: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut cur = String::new();
    for word in text.split_whitespace() {
        if !cur.is_empty() && cur.width() + 1 + word.width() > w {
            lines.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(word);
    }
    if !cur.is_empty() || lines.is_empty() {
        lines.push(cur);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn width_of(l: &Line) -> usize {
        l.spans.iter().map(|s| s.content.width()).sum()
    }

    #[test]
    fn boxes_are_rectangles_at_any_width() {
        for w in [42usize, 60, 80, 200] {
            let box_w = w.saturating_sub(2).clamp(MIN_BOX, MAX_BOX);
            let all = lines(w);
            let boxed: Vec<_> = all.iter().filter(|l| l.spans.len() > 1).collect();
            assert!(!boxed.is_empty());
            for l in boxed {
                assert_eq!(width_of(l), box_w + 2, "ragged row at width {w}: {l:?}");
            }
        }
    }

    #[test]
    fn every_command_and_key_is_listed() {
        let text: String = lines(80).iter().flat_map(|l| l.spans.iter().map(|s| s.content.to_string())).collect();
        for e in COMMANDS.iter().chain(KEYS) {
            assert!(text.contains(e.name), "{} missing", e.name);
        }
    }

    #[test]
    fn wraps_long_descriptions_inside_the_box() {
        let narrow = lines(44);
        assert!(narrow.len() > lines(100).len());
    }
}
