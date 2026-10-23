//! Key handling, the input line editor, and slash commands.

use super::app::{App, Block, Cmd, Kind};
use super::ask::Choice;
use super::theme;
use crate::config::Paths;
use crate::permission::Verdict;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

impl App {
    pub(super) fn insert(&mut self, s: &str) {
        self.input.insert_str(self.cursor, s);
        self.cursor += s.len();
    }

    pub(super) fn prev_boundary(&self) -> usize {
        self.input[..self.cursor].char_indices().last().map(|(i, _)| i).unwrap_or(0)
    }

    pub(super) fn next_boundary(&self) -> usize {
        self.input[self.cursor..].char_indices().nth(1).map(|(i, _)| self.cursor + i).unwrap_or(self.input.len())
    }
}

/// A key while a permission question is on screen: it takes every key, so
/// typing can't reach the input line or answer by accident. Returns true to quit.
fn prompt_key(app: &mut App, k: KeyEvent) -> bool {
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    // Interrupting is always allowed: the question goes with the turn.
    if k.code == KeyCode::Esc || (ctrl && k.code == KeyCode::Char('c')) {
        while !app.prompts.is_empty() {
            app.answer_prompt(|p| (p.answer(Verdict::Cancel), false));
        }
        if let Some(c) = &app.canceller {
            c.send_replace(true);
        }
        return false;
    }
    let Some(p) = app.prompts.front_mut() else { return false };
    if !p.ready() {
        return false;
    }
    let choices = p.choices();
    let pick = match k.code {
        KeyCode::Up | KeyCode::Char('k') => {
            p.selected = (p.selected + choices.len() - 1) % choices.len();
            None
        }
        KeyCode::Down | KeyCode::Left | KeyCode::Right | KeyCode::Char('j') => {
            p.selected = (p.selected + 1) % choices.len();
            None
        }
        KeyCode::Tab | KeyCode::BackTab => {
            p.toggle_folder();
            None
        }
        KeyCode::Enter => choices.get(p.selected).copied(),
        KeyCode::Char(c) if !ctrl => choices.iter().copied().find(|ch| ch.key() == c.to_ascii_lowercase()),
        _ => None,
    };
    if let Some(choice) = pick {
        app.answer_prompt(|p| (p.choose(choice), choice != Choice::Deny));
    }
    false
}

/// Returns true to quit.
pub(super) fn handle_key(app: &mut App, k: KeyEvent, cmd: &mpsc::UnboundedSender<Cmd>) -> bool {
    app.prune_prompts();
    if !app.prompts.is_empty() {
        return prompt_key(app, k);
    }
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    let alt = k.modifiers.contains(KeyModifiers::ALT);
    let shift = k.modifiers.contains(KeyModifiers::SHIFT);
    match k.code {
        KeyCode::Char('c') if ctrl => {
            if !app.input.is_empty() {
                app.input.clear();
                app.cursor = 0;
            } else if app.busy {
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
            if app.busy {
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
        KeyCode::Home => app.cursor = app.input[..app.cursor].rfind('\n').map(|i| i + 1).unwrap_or(0),
        KeyCode::End => {
            app.cursor = app.input[app.cursor..].find('\n').map(|i| app.cursor + i).unwrap_or(app.input.len())
        }
        KeyCode::Char('a') if ctrl => app.cursor = 0,
        KeyCode::Char('e') if ctrl => app.cursor = app.input.len(),
        KeyCode::Char('u') if ctrl => {
            app.input.replace_range(..app.cursor, "");
            app.cursor = 0;
        }
        KeyCode::Char('k') if ctrl => app.input.truncate(app.cursor),
        KeyCode::Up if !app.input.contains('\n') => {
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
        KeyCode::Down if !app.input.contains('\n') => {
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
    let before = &app.input[..app.cursor];
    let trimmed = before.trim_end();
    let start = trimmed.rfind(|c: char| c.is_whitespace()).map(|i| i + 1).unwrap_or(0);
    app.input.replace_range(start..app.cursor, "");
    app.cursor = start;
}

fn slash(app: &mut App, rest: &str, cmd: &mpsc::UnboundedSender<Cmd>) -> bool {
    let (name, arg) = rest.split_once(' ').map(|(a, b)| (a, b.trim())).unwrap_or((rest, ""));
    match name {
        "quit" | "exit" | "q" => return true,
        "new" | "clear" => {
            app.blocks.clear();
            app.cache.clear();
            app.usage = Default::default();
            app.context_pct = 0;
            app.groups.clear();
            let _ = cmd.send(Cmd::New);
        }
        "model" if !arg.is_empty() => {
            app.cfg.model = arg.to_string();
            let _ = cmd.send(Cmd::Model(arg.to_string()));
            app.notice(theme::ACCENT, format!("model → {arg}"));
        }
        "learn" => {
            let _ = cmd.send(Cmd::LearnNow);
        }
        "memory" => {
            let paths = Paths::resolve().ok();
            let block = paths.map(|p| crate::memory::Store::new(&p, &app.cfg.memory).with_project(&p, &std::env::current_dir().unwrap_or_default()).prompt_block()).unwrap_or_default();
            app.notice(theme::DIM, if block.is_empty() { "memory is empty".to_string() } else { block });
        }
        "skills" => {
            let list = Paths::resolve().map(|p| crate::skills::Library::new(&p).list()).unwrap_or_default();
            let text = if list.is_empty() {
                "no skills yet; the learning pass writes them as you work".to_string()
            } else {
                list.iter().map(|s| format!("• {} — {}", s.name, s.description)).collect::<Vec<_>>().join("\n")
            };
            app.notice(theme::DIM, text);
        }
        "help" | "?" => app.notice(
            theme::DIM,
            "/new  fresh session · /model <id>  switch model · /skills · /memory · /learn  run the learning pass now · /quit\nenter send · alt+enter or ctrl+j newline · esc interrupt · ctrl+o expand tool output · pgup/pgdn or wheel scroll · ctrl+c clear/quit",
        ),
        other => app.notice(theme::YELLOW, format!("unknown command /{other} — try /help")),
    }
    false
}
