//! The slash palette: typing `/` lists built-in commands and skills under the
//! input, narrowed with each keystroke, best match on top. The skill library
//! is read from disk whenever the query changes, so a skill written or edited
//! mid-session is in the list (and runnable) without a restart.

use super::help;
use super::theme;
use crate::skills::Skill;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

/// Rows shown at once; the list scrolls to keep the selection in view.
pub(super) const MAX_ROWS: usize = 8;

pub(super) struct Item {
    /// What gets typed, slash included.
    pub(super) name: String,
    pub(super) arg: &'static str,
    pub(super) about: String,
    pub(super) skill: bool,
}

impl Item {
    /// Picking it completes the name and waits for more input instead of
    /// running it: skills (the request follows) and commands with a required
    /// `<arg>`.
    pub(super) fn takes_input(&self) -> bool {
        self.skill || self.arg.starts_with('<')
    }
}

pub(super) struct Palette {
    /// The text after `/` the items were matched against.
    pub(super) query: String,
    pub(super) items: Vec<Item>,
    pub(super) selected: usize,
}

/// The query when `input` is a lone slash word (`/cre`), else none: once a
/// space or newline is typed the user is writing the request.
pub(super) fn query(input: &str) -> Option<&str> {
    input.strip_prefix('/').filter(|q| !q.contains(char::is_whitespace))
}

/// Built-ins and skills matching `q`, best first. A skill whose name is a
/// built-in's is left out: the built-in wins (`/skills <name>` still runs it).
pub(super) fn candidates(q: &str, skills: &[Skill]) -> Vec<Item> {
    let q = q.to_lowercase();
    let builtins = help::COMMANDS.iter().map(|e| Item {
        name: e.name.to_string(),
        arg: e.arg,
        about: e.about.to_string(),
        skill: false,
    });
    let skills = skills.iter().filter(|s| !help::is_builtin(&s.name)).map(|s| Item {
        name: format!("/{}", s.name),
        arg: "",
        about: s.description.clone(),
        skill: true,
    });
    let mut scored: Vec<((u8, usize), Item)> =
        builtins.chain(skills).filter_map(|i| score(&q, &i.name[1..], &i.about).map(|s| (s, i))).collect();
    scored.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.skill.cmp(&b.1.skill)).then(a.1.name.cmp(&b.1.name)));
    scored.into_iter().map(|(_, i)| i).collect()
}

/// Lower is better: (tier, tiebreak). Prefix of the name, then substring,
/// then the query's letters in order, then a substring of the description.
pub(super) fn score(q: &str, name: &str, about: &str) -> Option<(u8, usize)> {
    if q.is_empty() {
        return Some((0, 0));
    }
    if name.starts_with(q) {
        return Some((0, name.len()));
    }
    if let Some(i) = name.find(q) {
        return Some((1, i));
    }
    if let Some(span) = subsequence(q, name) {
        return Some((2, span));
    }
    about.to_lowercase().find(q).map(|i| (3, i))
}

/// How many chars of `hay` it takes to see every char of `q` in order.
fn subsequence(q: &str, hay: &str) -> Option<usize> {
    let mut want = q.chars().peekable();
    let mut start = None;
    for (i, c) in hay.chars().enumerate() {
        if want.peek() == Some(&c) {
            start.get_or_insert(i);
            want.next();
            if want.peek().is_none() {
                return Some(i + 1 - start.unwrap_or(0));
            }
        }
    }
    None
}

impl Palette {
    pub(super) fn selected(&self) -> Option<&Item> {
        self.items.get(self.selected)
    }

    pub(super) fn step(&mut self, down: bool) {
        let n = self.items.len().max(1);
        self.selected = if down { (self.selected + 1) % n } else { (self.selected + n - 1) % n };
    }

    /// At most `rows` (and `MAX_ROWS`) rows for a terminal `width`, scrolled
    /// to the selection: name column (commands in the accent color, skills
    /// green), then the description, dimmed and cut to fit.
    pub(super) fn lines(&self, width: usize, rows: usize) -> Vec<Line<'static>> {
        let mut rows = rows.clamp(1, MAX_ROWS);
        // One row says how many more there are, so the list never just stops.
        let hint = self.items.len() > rows && rows > 1;
        if hint {
            rows -= 1;
        }
        let first = self.selected.saturating_sub(rows - 1);
        let shown = &self.items[first..self.items.len().min(first + rows)];
        let label = |i: &Item| i.name.width() + if i.arg.is_empty() { 0 } else { 1 + i.arg.width() };
        let col = shown.iter().map(label).max().unwrap_or(0).min(width / 2);
        let mut out: Vec<Line<'static>> = shown
            .iter()
            .enumerate()
            .map(|(n, i)| {
                let sel = first + n == self.selected;
                let color = if i.skill { theme::GREEN } else { theme::ACCENT };
                let mut name = Style::new().fg(color);
                if sel {
                    name = name.add_modifier(Modifier::BOLD);
                }
                let mut spans = vec![
                    Span::styled(if sel { "  ▸ " } else { "    " }, Style::new().fg(theme::ACCENT2)),
                    Span::styled(i.name.clone(), name),
                ];
                if !i.arg.is_empty() {
                    spans.push(Span::styled(format!(" {}", i.arg), Style::new().fg(theme::YELLOW)));
                }
                let pad = col.saturating_sub(label(i)) + 2;
                spans.push(Span::raw(" ".repeat(pad)));
                let kind = if i.skill { "skill · " } else { "" };
                let room = width.saturating_sub(4 + col + 2 + kind.width());
                spans.push(Span::styled(kind, theme::faint()));
                spans.push(Span::styled(super::view::fit(&i.about, room), theme::dim()));
                Line::from(spans)
            })
            .collect();
        if hint {
            let hidden = self.items.len() - shown.len();
            out.push(Line::from(Span::styled(format!("    … {hidden} more (↑↓ to scroll)"), theme::faint())));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skill(name: &str, description: &str) -> Skill {
        Skill { name: name.into(), description: description.into(), dir: Default::default() }
    }

    fn names(q: &str, skills: &[Skill]) -> Vec<String> {
        candidates(q, skills).into_iter().map(|i| i.name).collect()
    }

    #[test]
    fn prefix_beats_substring_beats_letters_beats_description() {
        let lib = [
            skill("create-verification", "build a verification skill"),
            skill("recreate-db", "drop and rebuild the db"),
            skill("cargo-release", "publish a crate"),
            skill("notes", "create meeting notes"),
        ];
        let got = names("cre", &lib);
        assert_eq!(got[0], "/create-verification");
        let pos = |n: &str| got.iter().position(|g| g == n).unwrap();
        assert!(pos("/recreate-db") < pos("/cargo-release"), "{got:?}");
        assert!(pos("/cargo-release") < pos("/notes"), "{got:?}");
    }

    #[test]
    fn empty_query_lists_builtins_then_skills_and_builtins_win_a_name() {
        let lib = [skill("help", "a skill named like a command"), skill("deploy", "ship it")];
        let got = names("", &lib);
        assert_eq!(got.iter().filter(|n| *n == "/help").count(), 1);
        assert_eq!(got.last().map(String::as_str), Some("/deploy"));
        assert!(candidates("", &lib).iter().any(|i| i.name == "/help" && !i.skill));
    }

    #[test]
    fn query_stops_at_whitespace() {
        assert_eq!(query("/cre"), Some("cre"));
        assert_eq!(query("/"), Some(""));
        assert_eq!(query("/create-verification do it"), None);
        assert_eq!(query("hello"), None);
    }

    #[test]
    fn rows_fit_the_width_and_mark_the_selection() {
        let lib = [skill("create-verification", &"long description ".repeat(20))];
        let mut p = Palette { query: String::new(), items: candidates("", &lib), selected: 0 };
        p.step(false);
        let lines = p.lines(60, 20);
        assert!(lines.len() <= MAX_ROWS);
        assert!(p.lines(60, 3).last().is_some_and(|l| l.spans[0].content.contains("more")));
        for l in &lines {
            assert!(l.spans.iter().map(|s| s.content.width()).sum::<usize>() <= 60, "{l:?}");
        }
        let text: String = lines.iter().rev().nth(1).unwrap().spans.iter().map(|s| s.content.to_string()).collect();
        assert!(text.contains("▸ /create-verification"), "{text}");
    }
}
