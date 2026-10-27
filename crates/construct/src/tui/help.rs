//! The `/help` panel: commands, skills and keys as rounded boxes, drawn into
//! plain transcript lines. The tables below are the only place a command or
//! key is described (the slash palette lists `COMMANDS` too); the box,
//! columns and colors are derived from them, so a new entry needs one line
//! and nothing else.

use super::theme;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

/// One row: what to type, an optional argument shown after it, what it does.
pub(super) struct Entry {
    pub(super) name: &'static str,
    pub(super) arg: &'static str,
    pub(super) about: &'static str,
}

const fn entry(name: &'static str, arg: &'static str, about: &'static str) -> Entry {
    Entry { name, arg, about }
}

pub(super) const COMMANDS: &[Entry] = &[
    entry("/new", "", "fresh session (also /clear)"),
    entry("/model", "[id]", "pick from the catalog, or switch to any id"),
    entry("/models", "[query]", "search the endpoint's model list: window and price"),
    entry("/skills", "[name]", "list the skill library, or run one (also /<name> <request>)"),
    entry("/memory", "", "show what construct remembers"),
    entry("/learn", "", "run the learning pass now"),
    entry("/status", "", "session id, journal and totals"),
    entry("/copy", "[error|status]", "last reply (or last tool error, or status) to the clipboard"),
    entry("/help", "", "this list (also /?)"),
    entry("/quit", "", "exit (also /exit, /q)"),
];

const KEYS: &[Entry] = &[
    entry("enter", "", "send"),
    entry("/", "↑↓ tab", "commands and skills as you type; esc closes"),
    entry("alt+enter", "ctrl+j", "newline"),
    entry("esc", "", "interrupt"),
    entry("ctrl+o", "", "expand tool output"),
    entry("wheel", "↑↓ pgup pgdn", "scroll"),
    entry("ctrl+p", "ctrl+n", "input history"),
    entry("ctrl+c", "", "clear the input, then quit"),
];

/// Other names the built-ins answer to; with `COMMANDS` they shadow a skill
/// of the same name.
const ALIASES: &[&str] = &["clear", "exit", "q", "?"];

/// Whether `/name` is a built-in command, so a skill can't take the name.
pub(super) fn is_builtin(name: &str) -> bool {
    ALIASES.contains(&name) || COMMANDS.iter().any(|e| &e.name[1..] == name)
}

/// The skills box's rows as a `/help` block carries them: `name\tdescription`
/// per line, captured when `/help` ran.
pub(super) fn skill_rows(skills: &[crate::skills::Skill]) -> String {
    skills.iter().map(|s| format!("{}\t{}", s.name, s.description)).collect::<Vec<_>>().join("\n")
}

const NOTE: &str = "select text with the mouse (shift-drag if your terminal asks for it); /copy works over ssh";

/// Widest and narrowest box. Beyond the first, a wide terminal only spreads
/// the descriptions out; below the second the columns have no room to wrap.
const MAX_BOX: usize = 78;
const MIN_BOX: usize = 40;

/// The whole panel for a transcript `width`, indented to line up with notices.
/// `skills` is `skill_rows`' text; with none the skills box is left out.
pub(super) fn lines(width: usize, skills: &str) -> Vec<Line<'static>> {
    let box_w = width.saturating_sub(2).clamp(MIN_BOX, MAX_BOX);
    let rows = |t: &'static [Entry]| t.iter().map(|e| (e.name.to_string(), e.arg, e.about.to_string())).collect();
    let mut out = panel("commands", theme::ACCENT, rows(COMMANDS), theme::ACCENT, box_w);
    let skills: Vec<Row> =
        skills.lines().filter_map(|l| l.split_once('\t')).map(|(n, d)| (format!("/{n}"), "", d.to_string())).collect();
    if !skills.is_empty() {
        out.extend(panel("skills", theme::GREEN, skills, theme::GREEN, box_w));
    }
    out.extend(panel("keys", theme::ACCENT2, rows(KEYS), theme::CYAN, box_w));
    for l in words(NOTE, box_w.saturating_sub(2)) {
        out.push(Line::from(Span::styled(format!("   {l}"), theme::dim())));
    }
    out
}

/// A rounded box titled `title`, one aligned row per entry. `name` colors the
/// first column; an entry's `arg` (an argument, or alternate keys) is always
/// yellow, so it reads as a different kind of thing from the name.
/// One table row: name, argument, description.
type Row = (String, &'static str, String);

fn panel(title: &str, border: Color, entries: Vec<Row>, name: Color, box_w: usize) -> Vec<Line<'static>> {
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
    for e in &entries {
        for (i, text) in words(&e.2, about_w).into_iter().enumerate() {
            let mut row = vec![Span::raw("  "), Span::styled("│ ", edge)];
            let used = if i == 0 {
                row.push(Span::styled(e.0.clone(), Style::new().fg(name).add_modifier(Modifier::BOLD)));
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
            let all = lines(w, "deploy\tship it to production");
            let boxed: Vec<_> = all.iter().filter(|l| l.spans.len() > 1).collect();
            assert!(!boxed.is_empty());
            for l in boxed {
                assert_eq!(width_of(l), box_w + 2, "ragged row at width {w}: {l:?}");
            }
        }
    }

    #[test]
    fn every_command_and_key_is_listed() {
        let text: String =
            lines(80, "deploy\tship it").iter().flat_map(|l| l.spans.iter().map(|s| s.content.to_string())).collect();
        for e in COMMANDS.iter().chain(KEYS) {
            assert!(text.contains(e.name), "{} missing", e.name);
        }
        assert!(text.contains("╭─ skills") && text.contains("/deploy"));
        let none: String = lines(80, "").iter().flat_map(|l| l.spans.iter().map(|s| s.content.to_string())).collect();
        assert!(!none.contains("╭─ skills"));
    }

    #[test]
    fn wraps_long_descriptions_inside_the_box() {
        let narrow = lines(44, "");
        assert!(narrow.len() > lines(100, "").len());
    }
}
