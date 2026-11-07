//! Key handling, the input line editor, and slash commands. A `/name` that
//! isn't a built-in runs the skill of that name; built-ins win a clash.

use super::app::{App, Block, Cmd, Kind};
use super::commands::slash;
use super::picker::Outcome;
use super::theme;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

impl App {
    pub(super) fn insert(&mut self, s: &str) {
        self.input.insert_str(self.cursor, s);
        self.cursor += s.len();
    }

    #[expect(clippy::string_slice, reason = "the cursor only ever moves to a char boundary")]
    fn before_cursor(&self) -> &str {
        &self.input[..self.cursor]
    }

    #[expect(clippy::string_slice, reason = "the cursor only ever moves to a char boundary")]
    fn after_cursor(&self) -> &str {
        &self.input[self.cursor..]
    }

    pub(super) fn prev_boundary(&self) -> usize {
        self.before_cursor().char_indices().last().map(|(i, _)| i).unwrap_or(0)
    }

    pub(super) fn next_boundary(&self) -> usize {
        self.after_cursor().char_indices().nth(1).map(|(i, _)| self.cursor + i).unwrap_or(self.input.len())
    }
}

/// A key while the model picker is open: it takes every key, so typing
/// filters the list instead of reaching the input line.
fn picker_key(app: &mut App, k: KeyEvent, cmd: &mpsc::UnboundedSender<Cmd>) {
    let Some(p) = app.picker.as_mut() else { return };
    match p.key(k) {
        Outcome::Stay => {}
        Outcome::Close => app.picker = None,
        Outcome::Pick(id) => {
            app.picker = None;
            let _ = cmd.send(Cmd::Model(id));
        }
    }
}

/// A key while the slash palette is open. Up and down move the selection,
/// tab completes it, esc closes the palette. Enter completes a skill, a model
/// or a command that needs an argument, and otherwise lets the command run;
/// a model already completed is run by the next enter.
/// Returns true when the key was used up here.
fn palette_key(app: &mut App, k: KeyEvent) -> bool {
    let Some(p) = app.palette.as_mut() else { return false };
    let plain = !k.modifiers.intersects(KeyModifiers::ALT | KeyModifiers::SHIFT | KeyModifiers::CONTROL);
    match k.code {
        KeyCode::Up => p.step(false),
        KeyCode::Down => p.step(true),
        KeyCode::Esc => {
            app.palette_off = Some(app.input.clone());
            app.palette = None;
        }
        KeyCode::Tab | KeyCode::Enter if plain || k.code == KeyCode::Tab => {
            let Some(item) = p.selected() else { return false };
            let text = item.completion();
            let model = item.model;
            let done = model && app.input == text;
            // Enter on a plain command, or on a model that is already typed
            // out, falls through and sends it.
            let run = k.code == KeyCode::Enter && (!item.takes_input() || done);
            app.input = text;
            app.cursor = app.input.len();
            if model {
                // Complete means done narrowing: close until the input changes.
                app.palette_off = Some(app.input.clone());
                app.palette = None;
            }
            return !run;
        }
        _ => return false,
    }
    true
}

/// Returns true to quit.
pub(super) fn handle_key(app: &mut App, k: KeyEvent, cmd: &mpsc::UnboundedSender<Cmd>) -> bool {
    if let Some(p) = app.browser.as_mut() {
        let startup = p.startup;
        match p.key(k) {
            Outcome::Stay => {}
            Outcome::Close => {
                app.browser = None;
                return startup;
            }
            Outcome::Pick(id) => {
                app.browser = None;
                if !app.switch_blocked() {
                    app.turn.switch();
                    let _ = cmd.send(Cmd::Resume(Some(id)));
                }
            }
        }
        return false;
    }
    app.prune_prompts();
    if !app.prompts.is_empty() {
        return super::ask::prompt_key(app, k);
    }
    if app.picker.is_some() {
        picker_key(app, k, cmd);
        return false;
    }
    if app.turn.switching() {
        return false;
    }
    if app.peek_key(k) {
        return false;
    }
    if palette_key(app, k) {
        return false;
    }
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    let alt = k.modifiers.contains(KeyModifiers::ALT);
    let shift = k.modifiers.contains(KeyModifiers::SHIFT);
    match k.code {
        KeyCode::Char('c') if ctrl => {
            if !app.input.is_empty() {
                app.input.clear();
                app.cursor = 0;
            } else if app.turn.busy() {
                if let Some(c) = &app.canceller {
                    c.send_replace(true);
                }
            } else if app.quit_armed.is_some_and(|t| t.elapsed() < Duration::from_secs(2)) {
                return true;
            } else {
                app.quit_armed = Some(Instant::now());
                app.toast = Some(("press ctrl+c again to quit".into(), theme::YELLOW, Instant::now()));
            }
        }
        KeyCode::Char('d') if ctrl && app.input.is_empty() => return true,
        KeyCode::Esc => {
            if app.turn.busy() {
                if let Some(c) = &app.canceller {
                    c.send_replace(true);
                }
            } else {
                app.scroll = 0;
            }
        }
        KeyCode::Char('o') if ctrl => {
            app.expand = !app.expand;
        }
        KeyCode::Char('l') if ctrl => app.cache.clear(),
        KeyCode::Enter if alt || shift => app.insert("\n"),
        KeyCode::Char('j') if ctrl => app.insert("\n"),
        KeyCode::Enter => {
            // A trailing backslash continues the line, like a shell.
            if app.input.ends_with('\\') && app.cursor == app.input.len() {
                app.input.pop();
                app.cursor -= 1;
                app.insert("\n");
                return false;
            }
            let text = app.input.trim().to_string();
            if text.is_empty() {
                return false;
            }
            app.history.push(text.clone());
            app.hist_pos = None;
            app.input.clear();
            app.cursor = 0;
            app.scroll = 0;
            if let Some(rest) = text.strip_prefix('/') {
                return slash(app, rest, cmd);
            }
            app.turn.submit();
            app.push(Block::new(Kind::User, text.clone()));
            let _ = cmd.send(Cmd::Prompt(text));
        }
        KeyCode::Backspace if ctrl || alt => delete_word(app),
        KeyCode::Char('w') if ctrl => delete_word(app),
        KeyCode::Backspace => {
            if app.cursor > 0 {
                let p = app.prev_boundary();
                app.input.replace_range(p..app.cursor, "");
                app.cursor = p;
            }
        }
        KeyCode::Delete => {
            if app.cursor < app.input.len() {
                let n = app.next_boundary();
                app.input.replace_range(app.cursor..n, "");
            }
        }
        KeyCode::Left => app.cursor = app.prev_boundary(),
        KeyCode::Right => app.cursor = app.next_boundary(),
        KeyCode::Home => app.cursor = app.before_cursor().rfind('\n').map(|i| i + 1).unwrap_or(0),
        KeyCode::End => app.cursor = app.after_cursor().find('\n').map(|i| app.cursor + i).unwrap_or(app.input.len()),
        KeyCode::Char('a') if ctrl => app.cursor = 0,
        KeyCode::Char('e') if ctrl => app.cursor = app.input.len(),
        KeyCode::Char('u') if ctrl => {
            app.input.replace_range(..app.cursor, "");
            app.cursor = 0;
        }
        KeyCode::Char('k') if ctrl => app.input.truncate(app.cursor),
        // Up/Down scroll the transcript: with alternate-scroll the wheel arrives
        // as these keys and can't be told apart. History moves to ctrl+p/n.
        KeyCode::Up => app.scroll += 3,
        KeyCode::Down => app.scroll = app.scroll.saturating_sub(3),
        KeyCode::Char('p') if ctrl && !app.input.contains('\n') => {
            if app.history.is_empty() {
                return false;
            }
            let i = match app.hist_pos {
                Some(0) => 0,
                Some(i) => i - 1,
                None => app.history.len() - 1,
            };
            app.hist_pos = Some(i);
            app.input = app.history[i].clone();
            app.cursor = app.input.len();
        }
        KeyCode::Char('n') if ctrl && !app.input.contains('\n') => {
            if let Some(i) = app.hist_pos {
                if i + 1 < app.history.len() {
                    app.hist_pos = Some(i + 1);
                    app.input = app.history[i + 1].clone();
                } else {
                    app.hist_pos = None;
                    app.input.clear();
                }
                app.cursor = app.input.len();
            }
        }
        KeyCode::PageUp => app.scroll += 10,
        KeyCode::PageDown => app.scroll = app.scroll.saturating_sub(10),
        KeyCode::Char(c) if !ctrl => {
            let mut b = [0u8; 4];
            app.insert(c.encode_utf8(&mut b));
        }
        KeyCode::Tab => app.insert("    "),
        _ => {}
    }
    false
}

fn delete_word(app: &mut App) {
    let start = word_start(app.before_cursor());
    app.input.replace_range(start..app.cursor, "");
    app.cursor = start;
}

/// Where the last word of `before` starts: just past the space before it,
/// which may be wider than a byte (U+3000).
fn word_start(before: &str) -> usize {
    let trimmed = before.trim_end();
    trimmed.char_indices().rfind(|(_, c)| c.is_whitespace()).map_or(0, |(i, c)| i + c.len_utf8())
}

#[cfg(test)]
mod tests {
    use super::super::app::App;
    use super::handle_key;
    use crate::config::Config;
    use crate::text::prop;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
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
            // No '/': a line starting with one is a slash command, which reads the disk.
            4 => any::<char>().prop_map(|c| Act::Key(KeyCode::Char(if c == '/' || c.is_control() { 'x' } else { c }), KeyModifiers::NONE)),
            1 => prop::text().prop_map(|t| Act::Paste(t.replace('/', ""))),
        ]
    }

    #[test]
    fn the_cursor_follows_a_pasted_tab_as_drawn() {
        // Shrunk from `editing_keeps_the_cursor_on_a_char_boundary` (a `\0`):
        // a tab draws as nothing, so the cursor sits right after the `x`.
        let mut app = App::new(Config::default());
        app.insert("\tx");
        assert_eq!(app.input_lines(40).1, (0, 3));
    }

    #[test]
    fn an_emoji_keycap_wraps_at_its_drawn_width() {
        // Shrunk from `editing_keeps_the_cursor_on_a_char_boundary`: `0` and
        // U+FE0F draw two columns wide, one more than the chars count.
        let mut app = App::new(Config::default());
        app.insert(&format!("{}0\u{fe0f}", "x".repeat(27)));
        for l in app.input_lines(31).0 {
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
            let mut app = App::new(Config::default());
            let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
            for a in acts {
                match a {
                    Act::Key(k, m) => {
                        handle_key(&mut app, KeyEvent::new(k, m), &tx);
                    }
                    Act::Paste(t) => app.insert(&t.replace("\r\n", "\n").replace('\r', "\n")),
                }
                prop_assert!(app.input.is_char_boundary(app.cursor), "cursor {} in {:?}", app.cursor, app.input);
            }
            let (lines, (_, col)) = app.input_lines(width);
            let room = 2 + width.saturating_sub(3).max(4);
            for l in &lines {
                prop_assert!(prop::cols(l) <= room, "{} > {room}: {l:?}", prop::cols(l));
            }
            prop_assert!(usize::from(col) <= room, "cursor column {col} > {room}");
        }
    }

    #[test]
    fn word_start_steps_over_a_wide_space() {
        assert_eq!(super::word_start("one two  "), 4);
        assert_eq!(super::word_start("word"), 0);
        assert_eq!(super::word_start("一\u{3000}二"), "一\u{3000}".len());
    }
}
