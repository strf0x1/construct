//! The TUI's half of the question channel: [`Prompt`] is a question waiting
//! for a key, [`PromptQueue`] owns the FIFO queue and key handling, and
//! [`TuiAsk`] is how the agent's `Perms` puts one on screen.
//!
//! The design choice: the reply is a oneshot whose receiver lives inside the
//! asking tool call. If the call is cancelled the receiver is dropped, the
//! sender reports `is_closed`, and the UI removes the prompt: nothing has to
//! tell the UI a question went stale.

mod draw;

use crate::theme::GRACE;
use construct_core::permission::{Ask, Decision, Request, Verdict};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use std::cell::Cell;
use std::collections::VecDeque;
use std::time::Instant;
use tokio::sync::{mpsc, oneshot};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Choice {
    Once,
    Session,
    Always,
    Deny,
}

impl Choice {
    pub(super) fn key(self) -> char {
        match self {
            Choice::Once => 'y',
            Choice::Session => 's',
            Choice::Always => 'a',
            Choice::Deny => 'n',
        }
    }

    fn of(v: Verdict) -> Option<Choice> {
        match v {
            Verdict::Once => Some(Choice::Once),
            Verdict::Session => Some(Choice::Session),
            Verdict::Always => Some(Choice::Always),
            Verdict::Deny => Some(Choice::Deny),
            Verdict::Cancel => None,
        }
    }

    fn verdict(self) -> Verdict {
        match self {
            Choice::Once => Verdict::Once,
            Choice::Session => Verdict::Session,
            Choice::Always => Verdict::Always,
            Choice::Deny => Verdict::Deny,
        }
    }
}

pub(crate) struct Prompt {
    request: Request,
    reply: Option<oneshot::Sender<Decision>>,
    shown: Instant,
    /// Grant the folder rather than the path (a path request that has one).
    folder: bool,
    selected: usize,
    scroll: usize,
    scroll_limit: Cell<usize>,
}

impl Prompt {
    pub(crate) fn new(request: Request, reply: oneshot::Sender<Decision>) -> Self {
        Self {
            request,
            reply: Some(reply),
            shown: Instant::now(),
            folder: false,
            selected: 0,
            scroll: 0,
            scroll_limit: Cell::new(0),
        }
    }

    /// What can be answered, in the order shown (`Request::offers`).
    pub(super) fn choices(&self) -> Vec<Choice> {
        self.request.offers().into_iter().filter_map(Choice::of).collect()
    }

    /// The folder on offer, if there is one.
    pub(super) fn folder_offered(&self) -> Option<&str> {
        self.request.folder()
    }

    fn ready(&self) -> bool {
        self.shown.elapsed() >= GRACE
    }

    /// The asker gave up (its call was cancelled).
    fn stale(&self) -> bool {
        self.reply.as_ref().is_none_or(|r| r.is_closed())
    }

    fn toggle_folder(&mut self) {
        if self.folder_offered().is_some() {
            self.folder = !self.folder;
        }
    }

    /// Send the answer; what to say about it in the transcript.
    fn answer(&mut self, verdict: Verdict) -> String {
        let decision = Decision { verdict, folder: self.folder && self.folder_offered().is_some() };
        if let Some(r) = self.reply.take() {
            let _ = r.send(decision);
        }
        let what = self.summary();
        let how = match verdict {
            Verdict::Once => "allowed once",
            Verdict::Session => "allowed for this session",
            Verdict::Always => "always allowed (saved to config.toml)",
            Verdict::Deny => "denied",
            Verdict::Cancel => "cancelled",
        };
        format!("{how}: {what}")
    }

    fn choose(&mut self, c: Choice) -> String {
        self.answer(c.verdict())
    }

    /// One line on what was asked, for the transcript.
    fn summary(&self) -> String {
        match &self.request {
            Request::Path(p) => {
                let target = match (self.folder, &p.folder) {
                    (true, Some(f)) => f.clone(),
                    _ => p.paths.join(", "),
                };
                format!("{} access to {target} for {}", p.access.word(), p.tool)
            }
            Request::Operation(o) => format!("{} ({})", o.operation, o.scope),
        }
    }
}

/// What a key did to the permission prompt queue.
pub(crate) enum AskOutcome {
    /// No prompt was active.
    Inactive,
    /// Key was handled inside the prompt panel (navigation, scroll, or grace guard).
    Consumed,
    /// User answered the front prompt.
    Answered { text: String, allowed: bool },
    /// User pressed Esc or Ctrl+C: every queued prompt was cancelled; caller should cancel the turn.
    Cancelled(Vec<String>),
}

/// Questions waiting for a key, oldest first; the front one is on screen.
#[derive(Default)]
pub(crate) struct PromptQueue {
    prompts: VecDeque<Prompt>,
}

impl PromptQueue {
    pub(crate) fn push(&mut self, prompt: Prompt) {
        self.prompts.push_back(prompt);
    }

    pub(crate) fn clear(&mut self) {
        self.prompts.clear();
    }

    /// Drop questions whose call has gone (interrupted, timed out).
    pub(crate) fn prune(&mut self) {
        self.prompts.retain(|p| !p.stale());
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.prompts.is_empty()
    }

    pub(crate) fn panel_height(&self, width: u16, max: u16) -> Option<u16> {
        self.prompts.front().map(|p| p.height(width, max))
    }

    pub(crate) fn render_front(&self, f: &mut Frame, area: Rect, phase: f32) {
        if let Some(p) = self.prompts.front() {
            p.render(f, area, self.prompts.len() - 1, phase);
        }
    }

    /// Handle a key while a permission question may be on screen: while active
    /// it takes every key, so typing can't reach the input line or answer by accident.
    pub(crate) fn key(&mut self, k: KeyEvent) -> AskOutcome {
        self.prune();
        if self.prompts.is_empty() {
            return AskOutcome::Inactive;
        }
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        // Interrupting is always allowed: the question goes with the turn.
        if k.code == KeyCode::Esc || (ctrl && k.code == KeyCode::Char('c')) {
            let mut texts = Vec::with_capacity(self.prompts.len());
            while let Some(mut p) = self.prompts.pop_front() {
                texts.push(p.answer(Verdict::Cancel));
            }
            return AskOutcome::Cancelled(texts);
        }
        let Some(p) = self.prompts.front_mut() else {
            return AskOutcome::Inactive;
        };
        if !p.ready() {
            return AskOutcome::Consumed;
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
            KeyCode::PageUp => {
                p.scroll = p.scroll.min(p.scroll_limit.get()).saturating_sub(5);
                None
            }
            KeyCode::PageDown => {
                p.scroll = p.scroll.min(p.scroll_limit.get()).saturating_add(5).min(p.scroll_limit.get());
                None
            }
            KeyCode::Home => {
                p.scroll = 0;
                None
            }
            KeyCode::End => {
                p.scroll = p.scroll_limit.get();
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
        if let Some(choice) = pick
            && let Some(mut front) = self.prompts.pop_front()
        {
            let text = front.choose(choice);
            return AskOutcome::Answered { text, allowed: choice != Choice::Deny };
        }
        AskOutcome::Consumed
    }
}

/// What `Perms` asks through.
pub(crate) struct TuiAsk {
    pub(crate) tx: mpsc::UnboundedSender<Prompt>,
}

#[async_trait::async_trait]
impl Ask for TuiAsk {
    async fn ask(&self, request: Request) -> Decision {
        let (reply, answer) = oneshot::channel();
        if self.tx.send(Prompt::new(request, reply)).is_err() {
            return Decision::DENY;
        }
        // A prompt dropped unanswered (the UI went away) is not a yes.
        answer.await.unwrap_or(Decision::CANCEL)
    }
}
