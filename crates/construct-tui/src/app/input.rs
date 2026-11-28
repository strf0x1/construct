//! Top-level key routing: active overlays first (session browser, permission
//! prompts, peer popup/attach, model picker), then global shortcuts, then the
//! input composer and slash-command submission.

use super::App;
use crate::overlays::Outcome;
use crate::overlays::ask::AskOutcome;
use crate::theme;
use crate::transcript::{Item, Scroll};
use crate::worker::Cmd;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::time::Duration;
use tokio::sync::mpsc;

impl App {
    /// Handle one key press. Returns true to quit.
    pub(crate) fn handle_key(&mut self, k: KeyEvent, cmd: &mpsc::UnboundedSender<Cmd>) -> bool {
        if let Some(p) = self.browser.as_mut() {
            let startup = p.is_startup();
            match p.key(k) {
                Outcome::Stay => {}
                Outcome::Close => {
                    self.browser = None;
                    return startup;
                }
                Outcome::Pick(id) => {
                    self.browser = None;
                    if !self.switch_blocked() {
                        self.turn.switch();
                        let _ = cmd.send(Cmd::Resume(Some(id)));
                    }
                }
            }
            return false;
        }
        match self.prompts.key(k) {
            AskOutcome::Inactive => {}
            AskOutcome::Consumed => return false,
            AskOutcome::Answered { text, allowed } => {
                self.motion.answered(allowed);
                self.transcript
                    .push(Item::Notice(if allowed { theme::GREEN } else { theme::YELLOW }, format!("⚠ {text}")));
                return false;
            }
            AskOutcome::Cancelled(texts) => {
                for text in texts {
                    self.motion.answered(false);
                    self.transcript.push(Item::Notice(theme::YELLOW, format!("⚠ {text}")));
                }
                self.cancel();
                return false;
            }
        }
        if self.peers.key(k) {
            return false;
        }
        if let Some(p) = self.picker.as_mut() {
            match p.key(k) {
                Outcome::Stay => {}
                Outcome::Close => self.picker = None,
                Outcome::Pick(id) => {
                    self.picker = None;
                    let _ = cmd.send(Cmd::Model(id));
                }
            }
            return false;
        }
        if self.turn.switching() {
            return false;
        }
        if self.composer.palette_key(k) {
            return false;
        }
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match k.code {
            KeyCode::Char('c') if ctrl => {
                if !self.composer.is_empty() {
                    self.composer.clear();
                } else if self.turn.busy() {
                    if let Some(c) = &self.canceller {
                        c.send_replace(true);
                        self.quit_on_end = true;
                    }
                } else if self.quit_armed.is_some_and(|t| t.elapsed() < Duration::from_secs(2)) {
                    return true;
                } else {
                    self.arm_quit();
                }
            }
            KeyCode::Char('d') if ctrl && self.composer.is_empty() => return true,
            KeyCode::Esc => {
                if self.turn.busy() {
                    self.cancel();
                } else {
                    self.transcript.scroll_by(Scroll::Bottom);
                }
            }
            KeyCode::Char('o') if ctrl => {
                let heights = self.transcript.toggle_expand();
                self.motion.toggle_expand(heights);
            }
            KeyCode::Char('l') if ctrl => self.transcript.clear_cache(),
            // Up/Down scroll the transcript: with alternate-scroll the wheel arrives
            // as these keys and can't be told apart. History moves to ctrl+p/n.
            KeyCode::Up => self.transcript.scroll_by(Scroll::Up(3)),
            KeyCode::Down => self.transcript.scroll_by(Scroll::Down(3)),
            KeyCode::PageUp => self.transcript.scroll_by(Scroll::Up(10)),
            KeyCode::PageDown => self.transcript.scroll_by(Scroll::Down(10)),
            _ => {
                if let Some(text) = self.composer.edit_key(k) {
                    self.transcript.scroll_by(Scroll::Bottom);
                    if let Some(rest) = text.strip_prefix('/') {
                        return self.slash(rest, cmd);
                    }
                    self.turn.submit();
                    self.transcript.push(Item::User(text.clone()));
                    let _ = cmd.send(Cmd::Prompt(text));
                }
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use construct_core::config::Config;

    fn turn_end(interrupted: bool) -> construct_core::event::Event {
        construct_core::event::Event::TurnEnd {
            steps: 1,
            prompt: 0,
            completion: 0,
            ms: 5,
            interrupted,
            cached: 0,
            cost: None,
        }
    }

    /// Press `k` during a turn, then let it end as the agent reports.
    fn press_then_end(k: KeyEvent, interrupted: bool) -> (App, bool) {
        let mut app = App::new(Config::default());
        let (cancel, seen) = tokio::sync::watch::channel(false);
        app.canceller = Some(cancel);
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        app.on_event(construct_core::event::Event::TurnStart);
        assert!(!app.handle_key(k, &tx));
        let sent = *seen.borrow();
        app.on_event(turn_end(interrupted));
        (app, sent)
    }

    fn armed(app: &App) -> bool {
        app.quit_armed.is_some() && app.toast.as_ref().is_some_and(|t| t.0 == "press ctrl+c again to quit")
    }

    #[test]
    fn a_ctrl_c_the_finished_turn_ignored_arms_quit() {
        // The reply is drawn before `TurnEnd` arrives; a ctrl-c in that gap
        // sends a cancel the agent never reads, and the turn ends normally.
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        let (mut app, sent) = press_then_end(ctrl_c, false);
        assert!(sent, "ctrl-c during a turn sends the cancel");
        assert!(armed(&app), "the ctrl-c was lost: quit is not armed");
        // A second ctrl-c now quits.
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        assert!(app.handle_key(ctrl_c, &tx));
    }

    #[test]
    fn a_ctrl_c_that_interrupts_or_an_esc_does_not_arm_quit() {
        let (app, _) = press_then_end(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL), true);
        assert!(!armed(&app), "an interrupt also armed quit");
        let (app, sent) = press_then_end(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), false);
        assert!(sent && !armed(&app), "esc armed quit");
        // A cancel the agent dropped at the next `TurnStart` does not carry over.
        let mut app = App::new(Config::default());
        app.canceller = Some(tokio::sync::watch::channel(false).0);
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        app.on_event(construct_core::event::Event::TurnStart);
        app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL), &tx);
        app.on_event(turn_end(true));
        app.on_event(construct_core::event::Event::TurnStart);
        app.on_event(turn_end(false));
        assert!(!armed(&app), "an old ctrl-c armed quit at a later turn's end");
    }
}
