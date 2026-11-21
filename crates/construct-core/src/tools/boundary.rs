//! Deck's boundary endpoint as the engine sees it (ADR 0004 §6): a handle
//! the agent and the ACP server hold without knowing what is behind it.
//!
//! The design choice: this is a trait, not Deck's client. The engine needs
//! four things from it — the gateway sequence around a tool call, the late
//! decisions Deck pushed over ACP, and the "seen" bookkeeping — and a
//! `ToolSource` (the `construct-deck` crate) supplies the one implementation
//! and the tools that wait on it (#315). Its tools reach the concrete type
//! through [`BoundaryHost::as_any`]. Nothing here can allow anything.

use serde_json::Value;
use std::any::Any;

/// Late decisions ready for the model: the ids to mark seen once the note is
/// in the conversation, and the note itself.
pub struct Late {
    pub ids: Vec<String>,
    pub text: String,
}

/// One session's boundary endpoint.
#[async_trait::async_trait]
pub trait BoundaryHost: Send + Sync {
    /// The gateway sequence before a tool runs; `None` when it is unreachable.
    async fn cursor(&self) -> Option<u64>;
    /// What was blocked since `since`, worded for the model; `None` when
    /// nothing was or the endpoint is down. A dead endpoint costs the note,
    /// never the tool.
    async fn tool_note(&self, since: u64) -> Option<String>;
    /// A late decision Deck pushed over ACP, as its raw params, for the
    /// session `session`. Malformed or other-session notices are dropped.
    fn push_notice(&self, params: &Value, session: &str);
    /// Queued late decisions, once; `None` when there are none.
    fn take_late(&self) -> Option<Late>;
    /// The model has been told about `id`.
    fn mark_seen(&self, id: &str);
    /// The concrete handle, for the tools that share it.
    fn as_any(&self) -> &dyn Any;
}
