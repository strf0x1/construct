//! Slash command dispatch. A `/name` that isn't a built-in runs the skill of
//! that name; built-ins win a clash. Session changes are acknowledged by the
//! worker before the UI replaces any active state.

use super::{App, clip, listing};
use crate::help;
use crate::overlays::picker::Picker;
use crate::overlays::sessions::Browser;
use crate::theme;
use crate::transcript::Item;
use crate::worker::Cmd;
use construct_core::config::Paths;
use construct_core::llm::catalog::cached;
use tokio::sync::mpsc;

impl App {
    pub(super) fn slash(&mut self, rest: &str, cmd: &mpsc::UnboundedSender<Cmd>) -> bool {
        let (name, arg) = rest.split_once(' ').map(|(a, b)| (a, b.trim())).unwrap_or((rest, ""));
        match name {
            "quit" | "exit" | "q" => return true,
            "resume" => {
                if self.switch_blocked() {
                    return false;
                }
                if arg.is_empty() {
                    match Paths::resolve().and_then(|paths| Ok((paths, std::env::current_dir()?))) {
                        Ok((paths, cwd)) => {
                            self.browser = Some(Browser::open(paths, cwd, self.session.clone(), false));
                        }
                        Err(e) => {
                            self.transcript.push(Item::Notice(theme::RED, format!("Could not list sessions: {e:#}")));
                        }
                    }
                } else if arg != self.session {
                    self.turn.switch();
                    let _ = cmd.send(Cmd::Resume(Some(arg.to_string())));
                }
            }
            "rename" => {
                if arg.is_empty() {
                    self.transcript.push(Item::Notice(theme::YELLOW, "usage: /rename <non-empty title>".into()));
                } else {
                    let _ = cmd.send(Cmd::Rename(arg.to_string()));
                }
            }
            "new" | "clear" => {
                if self.switch_blocked() {
                    return false;
                }
                self.turn.switch();
                let _ = cmd.send(Cmd::New);
            }
            // The notice and the new limits come back as an `Event::Model` once
            // the agent has switched (or an error if it could not).
            "model" if !arg.is_empty() => {
                let _ = cmd.send(Cmd::Model(arg.to_string()));
            }
            "models" => {
                if let Err(e) = construct_core::llm::overlay::check_default_list(&self.cfg, &self.cfg.endpoint().name) {
                    self.transcript.push(Item::Notice(theme::DIM, e.to_string()));
                } else {
                    let models =
                        Paths::resolve().map(|p| cached(&p, &self.cfg.endpoint().base_url)).unwrap_or_default();
                    if models.is_empty() {
                        let none = "no model list cached yet: construct fetches OpenRouter's at startup, or run `construct models`";
                        self.transcript.push(Item::Notice(theme::DIM, none.into()));
                    } else {
                        match listing::models(&models, arg) {
                            Ok(item) => self.transcript.push(item),
                            Err(none) => self.transcript.push(Item::Notice(theme::DIM, none)),
                        }
                    }
                }
            }
            "model" => match Picker::open(&self.cfg) {
                Some(p) => self.picker = Some(p),
                None => {
                    let file = Paths::resolve().map(|p| p.models().display().to_string()).unwrap_or_default();
                    self.transcript.push(Item::Notice(
                        theme::DIM,
                        format!(
                            "no models in the catalog yet: add [models.\"<id>\"] entries (label, context_window, max_tokens) to {file}, or switch by id with /model <id>\nrunning: {}",
                            self.cfg.model
                        ),
                    ));
                }
            },
            "learn" => {
                let _ = cmd.send(Cmd::LearnNow);
            }
            "memory" => {
                let sections = Paths::resolve()
                    .map(|p| {
                        construct_core::memory::Store::new(&p, &self.cfg.memory)
                            .with_project(&p, &std::env::current_dir().unwrap_or_default())
                            .sections()
                    })
                    .unwrap_or_default();
                if sections.is_empty() {
                    self.transcript.push(Item::Notice(theme::DIM, "memory is empty".into()));
                } else {
                    self.transcript.push(listing::memory(sections));
                }
            }
            "skills" if !arg.is_empty() => {
                let (skill, text) = arg.split_once(' ').map(|(a, b)| (a, b.trim())).unwrap_or((arg, ""));
                self.run_skill(skill, text, rest, cmd);
            }
            "skills" => {
                let list = self.skills();
                if list.is_empty() {
                    self.transcript.push(Item::Notice(
                        theme::DIM,
                        "no skills yet; the learning pass writes them as you work".into(),
                    ));
                } else {
                    self.transcript.push(listing::skills(&list));
                }
            }
            "status" => match self.status_rows() {
                Ok(rows) => self.transcript.push(listing::status(&rows)),
                Err(why) => self.transcript.push(Item::Notice(theme::DIM, why)),
            },
            "copy" => self.copy(arg),
            "peer" => {
                if let Some((color, text)) = self.peers.command(arg) {
                    self.transcript.push(Item::Notice(color, text));
                }
            }
            "help" | "?" => {
                let skills = self.skills();
                self.transcript.push(Item::Help(help::skill_rows(&skills)));
            }
            other => self.run_skill(other, arg, rest, cmd),
        }
        false
    }

    /// `/<skill> text` (or `/skills <skill> text`): the line goes in the
    /// transcript as typed, and the agent runs a turn with the skill loaded.
    fn run_skill(&mut self, name: &str, text: &str, typed: &str, cmd: &mpsc::UnboundedSender<Cmd>) {
        if !self.skills().iter().any(|s| s.name == name) {
            self.transcript.push(Item::Notice(theme::YELLOW, format!("unknown command /{name} — try /help")));
            return;
        }
        self.turn.submit();
        self.transcript.push(Item::User(format!("/{typed}")));
        let _ = cmd.send(Cmd::Skill { name: name.to_string(), text: text.to_string(), original: format!("/{typed}") });
    }

    /// `/copy` puts the last assistant message on the clipboard; `/copy error` the
    /// last failed tool's output.
    fn copy(&mut self, arg: &str) {
        let (what, text) = match arg {
            "" => ("last reply", self.transcript.copy_text(false).map(str::to_string)),
            "error" => ("last tool error", self.transcript.copy_text(true).map(str::to_string)),
            "status" => ("status", self.status_rows().map(|rows| construct_core::journal::rows_text(&rows)).ok()),
            _ => {
                self.transcript.push(Item::Notice(theme::YELLOW, "usage: /copy · /copy error · /copy status".into()));
                return;
            }
        };
        match text.filter(|t| !t.trim().is_empty()) {
            None => self.transcript.push(Item::Notice(theme::YELLOW, format!("no {what} to copy"))),
            Some(t) => {
                let cut = clip::copy(&t);
                let note = if cut { ", cut to fit" } else { "" };
                self.transcript
                    .push(Item::Notice(theme::DIM, format!("copied {what} ({} chars{note})", t.chars().count())));
            }
        }
    }

    /// The `/status` rows, summed from this session's journal rows; `Err` is the
    /// sentence to show instead.
    fn status_rows(&self) -> Result<Vec<(&'static str, String)>, String> {
        let read = || -> anyhow::Result<Vec<(&'static str, String)>> {
            let paths = Paths::resolve()?;
            let file = construct_core::journal::path(&paths, &std::env::current_dir()?);
            let db = construct_core::journal::Journal::open(&file)?;
            let entries = db.since(&self.session, 0)?;
            let totals = construct_core::journal::Totals::of(&entries);
            Ok(construct_core::journal::status_rows(&self.session, &file, &self.cfg, &totals))
        };
        if self.session.is_empty() {
            return Err("session not started yet".to_string());
        }
        read().map_err(|e| format!("status unavailable: {e:#}"))
    }
}
