//! The parent-to-child and child-to-parent lines. Parent to child: one JSON
//! object per line, `start` first, then `prompt`s and `cancel`; closing stdin
//! ends the child. Child to parent: `Event` lines on stdout, nothing else
//! (stderr is for diagnostics).

use crate::config::Config;
use crate::event::Event;
use crate::sandbox::Policy;
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
