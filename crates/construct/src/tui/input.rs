//! Key handling, the input line editor, and slash commands. A `/name` that
//! isn't a built-in runs the skill of that name; built-ins win a clash.

use super::app::{App, Block, Cmd, Kind, Status};
use super::ask::Choice;
use super::picker::{Outcome, Picker};
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
    app.prune_prompts();
    if !app.prompts.is_empty() {
        return prompt_key(app, k);
    }
    if app.picker.is_some() {
        picker_key(app, k, cmd);
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
        // The notice and the new limits come back as an `Event::Model` once
        // the agent has switched (or an error if it could not).
        "model" if !arg.is_empty() => {
            let _ = cmd.send(Cmd::Model(arg.to_string()));
        }
        "models" => {
            let models =
                Paths::resolve().map(|p| crate::llm::catalog::cached(&p, &app.cfg.base_url)).unwrap_or_default();
            if models.is_empty() {
                let none =
                    "no model list cached yet: construct fetches OpenRouter's at startup, or run `construct models`";
                app.notice(theme::DIM, none);
            } else {
                match super::listing::models(&models, arg) {
                    Ok(block) => app.push(block),
                    Err(none) => app.notice(theme::DIM, none),
                }
            }
        }
        "model" => match Picker::open(&app.cfg) {
            Some(p) => app.picker = Some(p),
            None => {
                let file = Paths::resolve().map(|p| p.models().display().to_string()).unwrap_or_default();
                app.notice(
                    theme::DIM,
                    format!(
                        "no models in the catalog yet: add [models.\"<id>\"] entries (label, context_window, max_tokens) to {file}, or switch by id with /model <id>\nrunning: {}",
                        app.cfg.model
                    ),
                );
            }
        },
        "learn" => {
            let _ = cmd.send(Cmd::LearnNow);
        }
        "memory" => {
            let sections = Paths::resolve()
                .map(|p| {
                    crate::memory::Store::new(&p, &app.cfg.memory)
                        .with_project(&p, &std::env::current_dir().unwrap_or_default())
                        .sections()
                })
                .unwrap_or_default();
            if sections.is_empty() {
                app.notice(theme::DIM, "memory is empty");
            } else {
                app.push(super::listing::memory(sections));
            }
        }
        "skills" if !arg.is_empty() => {
            let (skill, text) = arg.split_once(' ').map(|(a, b)| (a, b.trim())).unwrap_or((arg, ""));
            run_skill(app, skill, text, rest, cmd);
        }
        "skills" => {
            let list = app.skills();
            if list.is_empty() {
                app.notice(theme::DIM, "no skills yet; the learning pass writes them as you work");
            } else {
                app.push(super::listing::skills(&list));
            }
        }
        "status" => match status(app) {
            Ok(rows) => app.push(super::listing::status(&rows)),
            Err(why) => app.notice(theme::DIM, why),
        },
        "copy" => copy(app, arg),
        "help" | "?" => {
            let skills = app.skills();
            app.push(Block::new(Kind::Help, super::help::skill_rows(&skills)));
        }
        other => run_skill(app, other, arg, rest, cmd),
    }
    false
}

/// `/<skill> text` (or `/skills <skill> text`): the line goes in the
/// transcript as typed, and the agent runs a turn with the skill loaded.
fn run_skill(app: &mut App, name: &str, text: &str, typed: &str, cmd: &mpsc::UnboundedSender<Cmd>) {
    if !app.skills().iter().any(|s| s.name == name) {
        return app.notice(theme::YELLOW, format!("unknown command /{name} — try /help"));
    }
    app.push(Block::new(Kind::User, format!("/{typed}")));
    let _ = cmd.send(Cmd::Skill { name: name.to_string(), text: text.to_string() });
}

/// `/copy` puts the last assistant message on the clipboard; `/copy error` the
/// last failed tool's output.
fn copy(app: &mut App, arg: &str) {
    let (what, text) = match arg {
        "" => ("last reply", app.blocks.iter().rev().find(|b| b.kind == Kind::Assistant).map(|b| b.text.clone())),
        "error" => (
            "last tool error",
            app.blocks.iter().rev().find(|b| b.kind == Kind::Tool && b.status == Status::Err).map(|b| b.text.clone()),
        ),
        "status" => ("status", status(app).map(|rows| crate::journal::rows_text(&rows)).ok()),
        _ => return app.notice(theme::YELLOW, "usage: /copy · /copy error · /copy status".to_string()),
    };
    match text.filter(|t| !t.trim().is_empty()) {
        None => app.notice(theme::YELLOW, format!("no {what} to copy")),
        Some(t) => {
            let cut = super::clip::copy(&t);
            let note = if cut { ", cut to fit" } else { "" };
            app.notice(theme::DIM, format!("copied {what} ({} chars{note})", t.chars().count()));
        }
    }
}

/// The `/status` rows, summed from this session's journal rows; `Err` is the
/// sentence to show instead.
fn status(app: &App) -> Result<Vec<(&'static str, String)>, String> {
    let read = || -> anyhow::Result<Vec<(&'static str, String)>> {
        let paths = Paths::resolve()?;
        let file = crate::journal::path(&paths, &std::env::current_dir()?);
        let entries = crate::journal::Journal::open(&file)?.since(&app.session, 0)?;
        let totals = crate::journal::Totals::of(&entries);
        Ok(crate::journal::status_rows(&app.session, &file, &app.cfg.model, app.cfg.context_window, &totals))
    };
    if app.session.is_empty() {
        return Err("session not started yet".to_string());
    }
    read().map_err(|e| format!("status unavailable: {e:#}"))
}
