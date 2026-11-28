//! The input line editor, prompt history and slash palette. Owns the cursor
//! invariant (always on a UTF-8 char boundary of `input`) so callers never
//! reach into string offsets or palette state directly.

mod line;
mod palette;

use construct_core::config::Config;
use construct_core::skills::Skill;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use palette::{Palette, Query};
use ratatui::text::Line;

#[derive(Default)]
pub(crate) struct Composer {
    input: String,
    cursor: usize,
    history: Vec<String>,
    hist_pos: Option<usize>,
    /// The slash palette, while the input is a lone `/word` or `/model <word>`.
    palette: Option<Palette>,
    /// The input esc closed the palette on; it stays closed until that changes.
    palette_off: Option<String>,
}

impl Composer {
    pub(crate) fn is_empty(&self) -> bool {
        self.input.is_empty()
    }

    pub(crate) fn clear(&mut self) {
        self.input.clear();
        self.cursor = 0;
    }

    pub(crate) fn insert(&mut self, s: &str) {
        self.input.insert_str(self.cursor, s);
        self.cursor += s.len();
    }

    /// Replace history after a session restore. On the first session, anything
    /// typed before initialization finished is preserved and appended.
    pub(crate) fn replace_history(&mut self, mut restored: Vec<String>, keep_draft: bool) {
        if keep_draft {
            restored.extend(std::mem::take(&mut self.history));
            self.history = restored;
        } else {
            self.input.clear();
            self.cursor = 0;
            self.history = restored;
            self.hist_pos = None;
        }
        self.palette = None;
    }

    /// Open, narrow or close the palette to match the input. The skill library
    /// is read only when the query changes.
    pub(crate) fn refresh_palette(&mut self, cfg: &Config, skills: impl FnOnce() -> Vec<Skill>) {
        let query = match palette::query(&self.input) {
            Some(q) if self.palette_off.as_deref() != Some(self.input.as_str()) => q,
            _ => {
                self.palette = None;
                return;
            }
        };
        let (q, models) = match query {
            Query::Command(q) => (q.to_string(), false),
            Query::Model(q) => (q.to_string(), true),
        };
        if self.palette.as_ref().is_some_and(|p| p.matches(&q, models)) {
            return;
        }
        let items = if models { palette::model_candidates(&q, cfg) } else { palette::candidates(&q, &skills()) };
        self.palette = (!items.is_empty()).then_some(Palette::new(q, models, items));
    }

    /// Drop any cached palette (for example after a turn that may have written a
    /// skill); returns true if a palette was open so the caller can rebuild it.
    pub(crate) fn invalidate_palette(&mut self) -> bool {
        self.palette.take().is_some()
    }

    pub(crate) fn palette_lines(&self, width: usize, rows: usize) -> Vec<Line<'static>> {
        self.palette.as_ref().map(|p| p.lines(width, rows)).unwrap_or_default()
    }

    /// A key while the slash palette is open. Up and down move the selection,
    /// tab completes it, esc closes the palette. Enter completes a skill, a
    /// model or a command that needs an argument, and otherwise lets the
    /// command run; a model already completed is run by the next enter.
    /// Returns true when the key was used up here.
    pub(crate) fn palette_key(&mut self, k: KeyEvent) -> bool {
        let Some(p) = self.palette.as_mut() else { return false };
        let plain = !k.modifiers.intersects(KeyModifiers::ALT | KeyModifiers::SHIFT | KeyModifiers::CONTROL);
        match k.code {
            KeyCode::Up => p.step(false),
            KeyCode::Down => p.step(true),
            KeyCode::Esc => {
                self.palette_off = Some(self.input.clone());
                self.palette = None;
            }
            KeyCode::Tab | KeyCode::Enter if plain || k.code == KeyCode::Tab => {
                let Some(item) = p.selected() else { return false };
                let text = item.completion();
                let model = item.is_model();
                let done = model && self.input == text;
                // Enter on a plain command, or on a model that is already typed
                // out, falls through and sends it.
                let run = k.code == KeyCode::Enter && (!item.takes_input() || done);
                self.input = text;
                self.cursor = self.input.len();
                if model {
                    // Complete means done narrowing: close until the input changes.
                    self.palette_off = Some(self.input.clone());
                    self.palette = None;
                }
                return !run;
            }
            _ => return false,
        }
        true
    }

    /// Apply a line-editing key; returns `Some(trimmed)` when Enter submits text.
    pub(crate) fn edit_key(&mut self, k: KeyEvent) -> Option<String> {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);
        match k.code {
            KeyCode::Enter if alt || shift => self.insert("\n"),
            KeyCode::Char('j') if ctrl => self.insert("\n"),
            KeyCode::Enter => {
                // A trailing backslash continues the line, like a shell.
                if self.input.ends_with('\\') && self.cursor == self.input.len() {
                    self.input.pop();
                    self.cursor -= 1;
                    self.insert("\n");
                    return None;
                }
                let text = self.input.trim().to_string();
                if text.is_empty() {
                    return None;
                }
                self.history.push(text.clone());
                self.hist_pos = None;
                self.input.clear();
                self.cursor = 0;
                return Some(text);
            }
            KeyCode::Backspace if ctrl || alt => self.delete_word(),
            KeyCode::Char('w') if ctrl => self.delete_word(),
            KeyCode::Backspace => {
                if self.cursor > 0 {
                    let p = self.prev_boundary();
                    self.input.replace_range(p..self.cursor, "");
                    self.cursor = p;
                }
            }
            KeyCode::Delete => {
                if self.cursor < self.input.len() {
                    let n = self.next_boundary();
                    self.input.replace_range(self.cursor..n, "");
                }
            }
            KeyCode::Left => self.cursor = self.prev_boundary(),
            KeyCode::Right => self.cursor = self.next_boundary(),
            KeyCode::Home => self.cursor = self.before_cursor().rfind('\n').map(|i| i + 1).unwrap_or(0),
            KeyCode::End => {
                self.cursor = self.after_cursor().find('\n').map(|i| self.cursor + i).unwrap_or(self.input.len());
            }
            KeyCode::Char('a') if ctrl => self.cursor = 0,
            KeyCode::Char('e') if ctrl => self.cursor = self.input.len(),
            KeyCode::Char('u') if ctrl => {
                self.input.replace_range(..self.cursor, "");
                self.cursor = 0;
            }
            KeyCode::Char('k') if ctrl => self.input.truncate(self.cursor),
            KeyCode::Char('p') if ctrl && !self.input.contains('\n') => {
                if self.history.is_empty() {
                    return None;
                }
                let i = match self.hist_pos {
                    Some(0) => 0,
                    Some(i) => i - 1,
                    None => self.history.len() - 1,
                };
                self.hist_pos = Some(i);
                self.input = self.history[i].clone();
                self.cursor = self.input.len();
            }
            KeyCode::Char('n') if ctrl && !self.input.contains('\n') => {
                if let Some(i) = self.hist_pos {
                    if i + 1 < self.history.len() {
                        self.hist_pos = Some(i + 1);
                        self.input = self.history[i + 1].clone();
                    } else {
                        self.hist_pos = None;
                        self.input.clear();
                    }
                    self.cursor = self.input.len();
                }
            }
            KeyCode::Char(c) if !ctrl => {
                let mut b = [0u8; 4];
                self.insert(c.encode_utf8(&mut b));
            }
            KeyCode::Tab => self.insert("    "),
            _ => {}
        }
        None
    }

    #[expect(clippy::string_slice, reason = "the cursor only ever moves to a char boundary")]
    fn before_cursor(&self) -> &str {
        &self.input[..self.cursor]
    }

    #[expect(clippy::string_slice, reason = "the cursor only ever moves to a char boundary")]
    fn after_cursor(&self) -> &str {
        &self.input[self.cursor..]
    }

    fn prev_boundary(&self) -> usize {
        self.before_cursor().char_indices().last().map(|(i, _)| i).unwrap_or(0)
    }

    fn next_boundary(&self) -> usize {
        self.after_cursor().char_indices().nth(1).map(|(i, _)| self.cursor + i).unwrap_or(self.input.len())
    }

    fn delete_word(&mut self) {
        let start = word_start(self.before_cursor());
        self.input.replace_range(start..self.cursor, "");
        self.cursor = start;
    }
}

/// Where the last word of `before` starts: just past the space before it,
/// which may be wider than a byte (U+3000).
fn word_start(before: &str) -> usize {
    let trimmed = before.trim_end();
    trimmed.char_indices().rfind(|(_, c)| c.is_whitespace()).map_or(0, |(i, c)| i + c.len_utf8())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::prop;
    use proptest::prelude::*;

    /// One user action: a key, or a bracketed paste.
    #[derive(Debug, Clone)]
    enum Act {
        Key(KeyCode, KeyModifiers),
        Paste(String),
    }

    fn act() -> impl Strategy<Value = Act> {
        let (none, ctrl, alt) = (KeyModifiers::NONE, KeyModifiers::CONTROL, KeyModifiers::ALT);
        let keys = [
            (KeyCode::Backspace, none),
            (KeyCode::Backspace, ctrl),
            (KeyCode::Delete, none),
            (KeyCode::Left, none),
            (KeyCode::Right, none),
            (KeyCode::Home, none),
            (KeyCode::End, none),
            (KeyCode::Enter, none),
            (KeyCode::Enter, alt),
            (KeyCode::Tab, none),
            (KeyCode::Char('a'), ctrl),
            (KeyCode::Char('e'), ctrl),
            (KeyCode::Char('u'), ctrl),
            (KeyCode::Char('k'), ctrl),
            (KeyCode::Char('w'), ctrl),
            (KeyCode::Char('j'), ctrl),
            (KeyCode::Char('p'), ctrl),
            (KeyCode::Char('n'), ctrl),
        ];
        prop_oneof![
            4 => proptest::sample::select(keys.to_vec()).prop_map(|(k, m)| Act::Key(k, m)),
            4 => any::<char>().prop_map(|c| Act::Key(KeyCode::Char(if c == '/' || c.is_control() { 'x' } else { c }), KeyModifiers::NONE)),
            1 => prop::text().prop_map(|t| Act::Paste(t.replace('/', ""))),
        ]
    }

    #[test]
    fn the_cursor_follows_a_pasted_tab_as_drawn() {
        // Shrunk from `editing_keeps_the_cursor_on_a_char_boundary` (a `\0`):
        // a tab draws as nothing, so the cursor sits right after the `x`.
        let mut c = Composer::default();
        c.insert("\tx");
        assert_eq!(c.lines(40, false).1, (0, 3));
    }

    #[test]
    fn an_emoji_keycap_wraps_at_its_drawn_width() {
        // Shrunk from `editing_keeps_the_cursor_on_a_char_boundary`: `0` and
        // U+FE0F draw two columns wide, one more than the chars count.
        let mut c = Composer::default();
        c.insert(&format!("{}0\u{fe0f}", "x".repeat(27)));
        for l in c.lines(31, false).0 {
            assert!(prop::cols(&l) <= 30, "{l:?}");
        }
    }

    proptest! {
        #![proptest_config(prop::config())]

        /// Any run of edits leaves the cursor on a char boundary of the
        /// input, and the input box wraps every row inside its width.
        #[test]
        fn editing_keeps_the_cursor_on_a_char_boundary(
            acts in proptest::collection::vec(act(), 0..24),
            width in 0usize..60,
        ) {
            let mut c = Composer::default();
            for a in acts {
                match a {
                    Act::Key(k, m) => {
                        c.edit_key(KeyEvent::new(k, m));
                    }
                    Act::Paste(t) => c.insert(&t.replace("\r\n", "\n").replace('\r', "\n")),
                }
                prop_assert!(c.input.is_char_boundary(c.cursor), "cursor {} in {:?}", c.cursor, c.input);
            }
            let (lines, (_, col)) = c.lines(width, false);
            let room = 2 + width.saturating_sub(3).max(4);
            for l in &lines {
                prop_assert!(prop::cols(l) <= room, "{} > {room}: {l:?}", prop::cols(l));
            }
            prop_assert!(usize::from(col) <= room, "cursor column {col} > {room}");
        }
    }

    #[test]
    fn word_start_steps_over_a_wide_space() {
        assert_eq!(word_start("one two  "), 4);
        assert_eq!(word_start("word"), 0);
        assert_eq!(word_start("一\u{3000}二"), "一\u{3000}".len());
    }
}
