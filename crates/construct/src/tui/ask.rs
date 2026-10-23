//! The TUI's half of the question channel: a [`Prompt`] is a question waiting
//! for a key, and [`TuiAsk`] is how the agent's `Perms` puts one on screen.
//!
//! The design choice: the reply is a oneshot whose receiver lives inside the
//! asking tool call. If the call is cancelled the receiver is dropped, the
//! sender reports `is_closed`, and the UI removes the prompt: nothing has to
//! tell the UI a question went stale.

use crate::permission::{Ask, Decision, Request, Verdict};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot};

/// Keys are ignored for this long after a prompt appears, so the Enter or `y`
/// someone was in the middle of typing can't answer it.
const GRACE: Duration = Duration::from_millis(450);

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

    fn verdict(self) -> Verdict {
        match self {
            Choice::Once => Verdict::Once,
            Choice::Session => Verdict::Session,
            Choice::Always => Verdict::Always,
            Choice::Deny => Verdict::Deny,
        }
    }
}

pub(super) struct Prompt {
    pub(super) request: Request,
    reply: Option<oneshot::Sender<Decision>>,
    shown: Instant,
    /// Grant the folder rather than the path (a path request that has one).
    pub(super) folder: bool,
    pub(super) selected: usize,
}

impl Prompt {
    fn new(request: Request, reply: oneshot::Sender<Decision>) -> Self {
        Self { request, reply: Some(reply), shown: Instant::now(), folder: false, selected: 0 }
    }

    /// What can be answered, in the order shown. Nothing is offered that the
    /// answer couldn't deliver: no "always" for a call with no kind to
    /// remember, no allow where the agent offered no allow option.
    pub(super) fn choices(&self) -> Vec<Choice> {
        match &self.request {
            Request::Path(p) if p.no_always => vec![Choice::Once, Choice::Session, Choice::Deny],
            Request::Path(_) => vec![Choice::Once, Choice::Session, Choice::Always, Choice::Deny],
            Request::Tool(t) if !t.can_allow => vec![Choice::Deny],
            Request::Tool(t) if t.kind.is_none() => vec![Choice::Once, Choice::Session, Choice::Deny],
            Request::Tool(_) => vec![Choice::Once, Choice::Session, Choice::Always, Choice::Deny],
        }
    }

    /// The folder on offer, if there is one.
    pub(super) fn folder_offered(&self) -> Option<&str> {
        match &self.request {
            Request::Path(p) => p.folder.as_deref(),
            Request::Tool(_) => None,
        }
    }

    pub(super) fn ready(&self) -> bool {
        self.shown.elapsed() >= GRACE
    }

    /// The asker gave up (its call was cancelled).
    pub(super) fn stale(&self) -> bool {
        self.reply.as_ref().is_none_or(|r| r.is_closed())
    }

    pub(super) fn toggle_folder(&mut self) {
        if self.folder_offered().is_some() {
            self.folder = !self.folder;
        }
    }

    /// Send the answer; what to say about it in the transcript.
    pub(super) fn answer(&mut self, verdict: Verdict) -> String {
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

    pub(super) fn choose(&mut self, c: Choice) -> String {
        self.answer(c.verdict())
    }

    /// One line on what was asked, for the transcript.
    pub(super) fn summary(&self) -> String {
        match &self.request {
            Request::Path(p) => {
                let target = match (self.folder, &p.folder) {
                    (true, Some(f)) => f.clone(),
                    _ => p.paths.join(", "),
                };
                let who = if p.agent.is_empty() { String::new() } else { format!("{}: ", p.agent) };
                format!("{who}{} access to {target} for {}", p.access.word(), p.tool)
            }
            Request::Tool(t) => format!("{}: {}", t.agent, t.title),
        }
    }
}

/// What `Perms` asks through.
pub(super) struct TuiAsk {
    pub(super) tx: mpsc::UnboundedSender<Prompt>,
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
