//! Slash command dispatch. Session changes are acknowledged by the worker
//! before the UI replaces any active state.

use super::app::{App, Block, Cmd, Kind, Status};
use super::picker::Picker;
use super::theme;
use crate::config::Paths;
use crate::llm::catalog::cached;
use tokio::sync::mpsc;

pub(super) fn slash(app: &mut App, rest: &str, cmd: &mpsc::UnboundedSender<Cmd>) -> bool {
    let (name, arg) = rest.split_once(' ').map(|(a, b)| (a, b.trim())).unwrap_or((rest, ""));
    match name {
        "quit" | "exit" | "q" => return true,
        "resume" => {
            if app.switch_blocked() {
                return false;
            }
            if arg.is_empty() {
                match Paths::resolve().and_then(|paths| Ok((paths, std::env::current_dir()?))) {
                    Ok((paths, cwd)) => {
                        app.browser = Some(super::sessions::Browser::open(paths, cwd, app.session.clone(), false))
                    }
                    Err(e) => app.notice(theme::RED, format!("Could not list sessions: {e:#}")),
                }
            } else if arg != app.session {
                app.turn.switch();
                let _ = cmd.send(Cmd::Resume(Some(arg.to_string())));
            }
        }
        "rename" => {
            if arg.is_empty() {
                app.notice(theme::YELLOW, "usage: /rename <non-empty title>");
            } else {
                let _ = cmd.send(Cmd::Rename(arg.to_string()));
            }
        }
        "new" | "clear" => {
            if app.switch_blocked() {
                return false;
            }
            app.turn.switch();
            let _ = cmd.send(Cmd::New);
        }
        // The notice and the new limits come back as an `Event::Model` once
        // the agent has switched (or an error if it could not).
        "model" if !arg.is_empty() => {
            let _ = cmd.send(Cmd::Model(arg.to_string()));
        }
        "models" => {
            let models = Paths::resolve().map(|p| cached(&p, &app.cfg.endpoint().base_url)).unwrap_or_default();
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
    app.turn.submit();
    app.push(Block::new(Kind::User, format!("/{typed}")));
    let _ = cmd.send(Cmd::Skill { name: name.to_string(), text: text.to_string(), original: format!("/{typed}") });
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
        let db = crate::journal::Journal::open(&file)?;
        let entries = db.since(&app.session, 0)?;
        let totals = crate::journal::Totals::session(&db, &entries)?;
        Ok(crate::journal::status_rows(&app.session, &file, &app.cfg, &totals))
    };
    if app.session.is_empty() {
        return Err("session not started yet".to_string());
    }
    read().map_err(|e| format!("status unavailable: {e:#}"))
}
