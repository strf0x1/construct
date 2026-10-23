//! How a parent talks to a child, as a state machine over lines ([`Wire`]),
//! and construct's own transport. Parent to child: one JSON object per line,
//! `start` first, then `prompt`s and `cancel`; closing stdin ends the child.
//! Child to parent: `Event` lines on stdout, nothing else (stderr is for
//! diagnostics). `acp::client` is the other `Wire`.
//!
//! The design choice: a `Wire` does no I/O. It is given lines and returns
//! lines, so `child::run` owns the process, the timeout and the kill whatever
//! the protocol, and a protocol can't leave a child running.

use super::run::Outcome;
use crate::config::Config;
use crate::event::Event;
use crate::permission::{Decision, Request};
use crate::sandbox::Policy;
use crate::tools::Call;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// What makes a session a child's: the prompt it runs under and who
/// delegated to it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Origin {
    /// The sandbox the child's whole process runs under, which its own file
    /// tools check against too, so a refusal names the real boundary.
    pub policy: Policy,
    pub system: String,
    pub parent_session: String,
    pub parent_call_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToChild {
    /// The child's whole setup. Secrets travel in the environment, never here.
    Start {
        config: Box<Config>,
        cwd: PathBuf,
        origin: Box<Origin>,
    },
    Prompt {
        text: String,
    },
    /// Stop the turn in flight, as the user's Esc does.
    Cancel,
    /// The parent's answer to the child's `Event::Ask` named `id`.
    Answer {
        id: String,
        decision: Decision,
    },
}

impl ToChild {
    pub fn line(&self) -> String {
        format!("{}\n", serde_json::to_string(self).unwrap_or_default())
    }

    pub fn parse(line: &str) -> Option<ToChild> {
        serde_json::from_str(line).ok()
    }
}

pub fn event_line(ev: &Event) -> String {
    format!("{}\n", serde_json::to_string(ev).unwrap_or_default())
}

/// A line of the child's stdout. Unknown event types are skipped, as every
/// reader of the vocabulary does.
pub fn parse_event(line: &str) -> Option<Event> {
    serde_json::from_str(line).ok()
}

/// A question the child asked that only the user can answer. The wire holds
/// the way to turn the answer into lines; `child::run` fetches the answer
/// (bounded by the delegation's timeout and cancel) and writes them.
pub struct Pending {
    pub request: Request,
    pub reply: Replier,
}

pub type Replier = Box<dyn FnOnce(Decision) -> Reply + Send>;

impl std::fmt::Debug for Pending {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pending").field("request", &self.request).finish_non_exhaustive()
    }
}

/// What an answer turns into: lines for the child, and events for the record.
#[derive(Default)]
pub struct Reply {
    pub lines: Vec<String>,
    pub events: Vec<Event>,
}

/// One delegation's conversation with its child.
pub trait Wire: Send {
    /// The lines to write once the child is running.
    fn open(&mut self) -> Vec<String>;
    /// One line the child wrote: pass its events up through `call`, note what
    /// the parent keeps in `out` (setting `out.finished` when the turn is
    /// over), and return any lines to write back.
    fn line(&mut self, line: &str, call: &Call, out: &mut Outcome) -> Vec<String>;
    /// The lines that stop the turn in flight.
    fn cancel(&mut self) -> Vec<String>;
}

/// construct's own transport: `start` and the task, then `Event` lines.
pub struct Jsonl {
    start: Option<ToChild>,
    task: String,
}

impl Jsonl {
    pub fn new(start: ToChild, task: &str) -> Self {
        Self { start: Some(start), task: task.to_string() }
    }
}

impl Wire for Jsonl {
    fn open(&mut self) -> Vec<String> {
        let mut lines: Vec<String> = self.start.take().iter().map(ToChild::line).collect();
        lines.push(ToChild::Prompt { text: self.task.clone() }.line());
        lines
    }

    fn line(&mut self, line: &str, call: &Call, out: &mut Outcome) -> Vec<String> {
        match parse_event(line) {
            // The child asks its parent; the answer goes back as a line.
            Some(Event::Ask { id, request }) => {
                let reply =
                    move |decision| Reply { lines: vec![ToChild::Answer { id, decision }.line()], events: vec![] };
                out.asks.push(Pending { request, reply: Box::new(reply) });
            }
            Some(ev) => {
                out.note(&ev);
                call.forward(ev);
            }
            None => {}
        }
        vec![]
    }

    fn cancel(&mut self) -> Vec<String> {
        vec![ToChild::Cancel.line()]
    }
}
