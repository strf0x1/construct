//! The one event vocabulary. What the agent emits, what `--json` prints, what
//! crosses the wire, and what a journal row's payload holds are this type
//! (docs/adr/0008).
//!
//! The rule a reader must know: it is additive-only. New variants and new
//! fields (with `#[serde(default)]`) are fine; renaming a variant or field, or
//! changing what one means, is not. Readers skip variants they don't know and
//! ignore fields they don't know, so an old binary can read a new journal.
//! Message-bearing variants carry the model-facing `llm::Message` verbatim so
//! resume replays exactly what the model saw.

use crate::llm::Message;
use crate::manifest::Manifest;
use crate::tools::OutputKind;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub type Events = tokio::sync::mpsc::UnboundedSender<Event>;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    /// A session's first event: where and what it runs as. Sessions are the
    /// rows sharing an id, so this is all there is of a session record.
    Head {
        id: String,
        harness: String,
        version: String,
        cwd: String,
        model: String,
        manifest: Manifest,
        /// Set for a child session (#32): who delegated, and through which call.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_session: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_call_id: Option<String>,
    },
    /// The system prompt the session started with.
    System {
        message: Message,
    },
    /// What the model was sent as the user's turn (with any skill reminder).
    User {
        message: Message,
    },
    /// The model's answer, with any tool calls.
    Assistant {
        message: Message,
    },
    /// A tool result as the model received it. `id` is the call id, so a
    /// result is one indexed lookup away.
    ToolResult {
        id: String,
        message: Message,
    },
    /// One model request: what it cost to send. `bytes` and `n_tools` are the
    /// numbers the golden snapshots pin.
    Request {
        model: String,
        bytes: u64,
        n_tools: u64,
        tools: Vec<String>,
    },
    TurnStart,
    /// What is running: build, config hash, tools. Once, on the first turn.
    Manifest(Manifest),
    Route {
        groups: Vec<String>,
        skills: Vec<String>,
        source: String,
        ms: u64,
        note: String,
    },
    /// The model's response stream opened.
    Opened,
    Text {
        text: String,
    },
    Reasoning {
        text: String,
    },
    /// The model has started emitting a tool call (arguments still streaming).
    ToolPending {
        name: String,
    },
    ToolStart {
        id: String,
        name: String,
        args: String,
    },
    ToolEnd {
        id: String,
        name: String,
        ok: bool,
        output: String,
        ms: u64,
        /// How the UI renders `output`.
        kind: OutputKind,
        /// The tool's machine-readable result, for logs and scorers.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        data: Option<Value>,
        /// Stable error code when `ok` is false.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        code: Option<String>,
    },
    Usage {
        prompt: u64,
        completion: u64,
        cached: u64,
    },
    /// History was cut to fit the window. `before`/`after` count messages;
    /// the token fields are the planner's estimates. `cut` is what the
    /// projector replays on resume (absent in journals from before it).
    Compacted {
        before: usize,
        after: usize,
        /// "elide" or "summarize".
        #[serde(default)]
        mode: String,
        /// "threshold", "overflow" or "age".
        #[serde(default)]
        reason: String,
        #[serde(default)]
        tokens_before: u64,
        #[serde(default)]
        tokens_after: u64,
        /// The prompt budget planned against: window less output reserve.
        #[serde(default)]
        budget: u64,
        /// Whether the cut reached the target.
        #[serde(default)]
        reached: bool,
        /// How long the run was blocked, summarizer included.
        #[serde(default)]
        ms: u64,
        /// The summarizer's own usage, when it ran.
        #[serde(default)]
        summary_prompt: u64,
        #[serde(default)]
        summary_completion: u64,
        /// Why a summary was planned but not written (the cut fell back to eliding).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fallback: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cut: Option<crate::compact::Cut>,
    },
    TurnEnd {
        steps: u32,
        prompt: u64,
        completion: u64,
        ms: u64,
        interrupted: bool,
    },
    Error {
        message: String,
    },
    Learn {
        status: String,
    },
}

impl Event {
    /// Whether the journal keeps it. Stream deltas are not kept: the
    /// assistant message that follows holds the whole text, and a row per
    /// token would be most of the journal.
    pub fn durable(&self) -> bool {
        !matches!(
            self,
            Event::Opened
                | Event::Text { .. }
                | Event::Reasoning { .. }
                | Event::ToolPending { .. }
                | Event::Learn { .. }
        )
    }

    /// The model-facing message this event carries, if any. This is the
    /// whole projector's vocabulary: resume rebuilds context from these.
    pub fn message(&self) -> Option<&Message> {
        match self {
            Event::System { message }
            | Event::User { message }
            | Event::Assistant { message }
            | Event::ToolResult { message, .. } => Some(message),
            _ => None,
        }
    }

    /// The event for a message the agent appends to the conversation.
    pub fn for_message(message: Message) -> Event {
        match message.role.as_str() {
            "system" => Event::System { message },
            "assistant" => Event::Assistant { message },
            "tool" => Event::ToolResult { id: message.tool_call_id.clone().unwrap_or_default(), message },
            _ => Event::User { message },
        }
    }
}
