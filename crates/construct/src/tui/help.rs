//! The `/help` panel: commands, skills and keys as rounded boxes (`boxed.rs`).
//! The tables below are the only place a command or key is described (the
//! slash palette lists `COMMANDS` too); the box, columns and colors are
//! derived from them, so a new entry needs one line and nothing else.

use super::boxed::{self, Row, Section};
use super::theme;
use ratatui::text::Line;

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

/// The whole panel for a transcript `width`, indented to line up with notices.
/// `skills` is `skill_rows`' text; with none the skills box is left out.
pub(super) fn lines(width: usize, skills: &str) -> Vec<Line<'static>> {
    let rows = |t: &'static [Entry]| t.iter().map(|e| (e.name.to_string(), e.arg, e.about.to_string())).collect();
    let mut boxes = vec![Section::new("commands", theme::ACCENT, rows(COMMANDS))];
    let skills: Vec<Row> =
        skills.lines().filter_map(|l| l.split_once('\t')).map(|(n, d)| (format!("/{n}"), "", d.to_string())).collect();
    if !skills.is_empty() {
        boxes.push(Section::new("skills", theme::GREEN, skills));
    }
    boxes.push(Section { name: theme::CYAN, ..Section::new("keys", theme::ACCENT2, rows(KEYS)) });
    boxed::lines(width, boxed::NARROW, &boxes, NOTE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use unicode_width::UnicodeWidthStr;

    fn width_of(l: &Line) -> usize {
        l.spans.iter().map(|s| s.content.width()).sum()
    }

    #[test]
    fn boxes_are_rectangles_at_any_width() {
        for w in [42usize, 60, 80, 200] {
            let box_w = boxed::box_width(w, boxed::NARROW);
            let all = lines(w, "deploy\tship it to production");
            let framed: Vec<_> = all.iter().filter(|l| l.spans.len() > 1).collect();
            assert!(!framed.is_empty());
            for l in framed {
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
