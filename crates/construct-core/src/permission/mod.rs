//! Asking the person at the keyboard before a tool goes past its boundary, and
//! remembering the answer.
//!
//! One channel serves every kind of question: a tool call blocked by the
//! sandbox (a file tool, or a shell command that failed with a Landlock
//! denial), and a frozen developer operation. A question is a [`Request`];
//! whoever can reach the user implements [`Ask`] (the TUI and an ACP client
//! do; headless runs get [`NoAsk`], which denies, as they always have).
//!
//! The design choice: [`Perms`] sits between the two and is the only place an
//! answer takes effect: it checks what is already granted (the session, then
//! config), asks only when it must, applies the answer (once, for the session,
//! or written to `config.toml`), and records it.

mod grants;
mod named;
mod operation;
mod paths;
mod perms;
mod persist;

pub use grants::protected;
pub use named::{Outside, PolicyTools, outside};
pub use paths::blocked_in;
pub use perms::Perms;

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
    /// Asked before a shell command runs, for the paths it names (#64).
    #[serde(default)]
    pub ahead: bool,
}

/// A frozen developer operation awaiting explicit approval.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationAsk {
    pub operation: String,
    pub scope: String,
    /// The complete frozen execution plan, without credentials.
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    Path(PathAsk),
    Operation(OperationAsk),
}

impl Request {
    /// The answers on offer, in the order shown. Nothing is offered that the
    /// answer couldn't deliver: an operation's grant is never written down.
    pub fn offers(&self) -> Vec<Verdict> {
        use Verdict::{Always, Deny, Once, Session};
        match self {
            Request::Operation(_) => vec![Once, Session, Deny],
            Request::Path(_) => vec![Once, Session, Always, Deny],
        }
    }

    /// The folder on offer, if there is one.
    pub fn folder(&self) -> Option<&str> {
        match self {
            Request::Path(p) => p.folder.as_deref(),
            Request::Operation(_) => None,
        }
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
