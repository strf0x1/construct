//! Asking the person at the keyboard before a tool goes past its boundary, and
//! remembering the answer.
//!
//! One channel serves every kind of question: a tool call blocked by the
//! sandbox (a file tool, or a shell command that failed with a Landlock
//! denial), and a delegated ACP agent's `session/request_permission`. A
//! question is a [`Request`]; whoever can reach the user implements [`Ask`]
//! (the TUI does; headless runs get [`NoAsk`], which denies, as they always
//! have). [`Perms`] sits between the two: it checks what is already granted
//! (the session, then config), asks only when it must, applies the answer
//! (once, for the session, or written to `config.toml`), and records it.
//!
//! The design choice: the process that can reach the user decides, and a child
//! asks upward. A delegated construct child's questions travel to its parent as
//! `Event::Ask` and come back as `ToChild::Answer`, so a grant for a child is
//! kept, and written to `[agents.<name>]`, by the parent, whose config and
//! home are the real ones.

mod grants;
mod paths;
mod perms;
mod persist;
mod upstream;

pub use grants::protected;
pub use paths::blocked_in;
pub use perms::Perms;
pub use upstream::Upstream;

use serde::{Deserialize, Serialize};

/// What a path is wanted for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Access {
    Read,
    Write,
}

impl Access {
    pub fn word(self) -> &'static str {
        match self {
            Access::Read => "read",
            Access::Write => "write",
        }
    }
}

/// A tool wants a path the sandbox keeps it from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathAsk {
    /// The delegated agent asking; empty for the session's own agent.
    #[serde(default)]
    pub agent: String,
    pub tool: String,
    pub access: Access,
    /// Real paths, as the sandbox sees them.
    pub paths: Vec<String>,
    /// The folder holding the one path asked about, when approving the
    /// folder is a sound choice (see `grants::folder_for`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
    /// What the tool was doing (a shell command).
    #[serde(default)]
    pub detail: String,
    /// Approving runs the whole call again (a shell command found out too late).
    #[serde(default)]
    pub rerun: bool,
    /// "Always" is not on offer: it would write a child's definition wider
    /// than the session's own sandbox.
    #[serde(default)]
    pub no_always: bool,
}

/// A delegated agent asks to run one of its own tool calls.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolAsk {
    pub agent: String,
    pub call_id: String,
    pub title: String,
    /// ACP's tool kind (`execute`, `edit`, ...), when the agent said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Which of the two answers the agent offered an option for.
    pub can_allow: bool,
    pub can_reject: bool,
}

impl ToolAsk {
    /// What "for this session" and "always" remember: the kind when there is
    /// one (a whole class of calls), else this call's exact title.
    pub fn key(&self) -> String {
        match &self.kind {
            Some(k) => format!("kind:{k}"),
            None => format!("title:{}", self.title),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    Path(PathAsk),
    Tool(ToolAsk),
}

impl Request {
    /// Name the delegated agent this came from (a parent, relaying a child's).
    pub fn from_agent(mut self, name: &str) -> Self {
        match &mut self {
            Request::Path(p) => p.agent = name.to_string(),
            Request::Tool(t) => t.agent = name.to_string(),
        }
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// This one call only.
    Once,
    /// Until the session ends.
    Session,
    /// Written to `config.toml`.
    Always,
    Deny,
    /// The question stopped mattering (the turn was interrupted).
    Cancel,
}

impl Verdict {
    pub fn allows(self) -> bool {
        matches!(self, Verdict::Once | Verdict::Session | Verdict::Always)
    }

    pub fn word(self) -> &'static str {
        match self {
            Verdict::Once => "once",
            Verdict::Session => "session",
            Verdict::Always => "always",
            Verdict::Deny => "deny",
            Verdict::Cancel => "cancelled",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Decision {
    pub verdict: Verdict,
    /// Grant the request's `folder` rather than the path itself.
    #[serde(default)]
    pub folder: bool,
}

impl Decision {
    pub const DENY: Decision = Decision { verdict: Verdict::Deny, folder: false };
    pub const CANCEL: Decision = Decision { verdict: Verdict::Cancel, folder: false };
}

/// Whoever can put a question to the user.
#[async_trait::async_trait]
pub trait Ask: Send + Sync {
    async fn ask(&self, req: Request) -> Decision;
    /// False when nobody will answer, so the caller denies without recording
    /// a decision nobody made.
    fn interactive(&self) -> bool {
        true
    }
}

/// Headless: the answer is no, as it was before there was a question.
pub struct NoAsk;

#[async_trait::async_trait]
impl Ask for NoAsk {
    async fn ask(&self, _: Request) -> Decision {
        Decision::DENY
    }
    fn interactive(&self) -> bool {
        false
    }
}
