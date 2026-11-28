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
        /// Only journals from before delegation was removed (#294) have these;
        /// they stay so those sessions still read as children.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_session: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_call_id: Option<String>,
        /// How this session started (`None` for ordinary interactive sessions, ADR 0079).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        origin: Option<Box<payload::SessionOrigin>>,
    },
    /// The system prompt the session started with.
    System {
        message: Message,
    },
    /// What the model was sent as the user's turn (with any skill reminder).
    User {
        message: Message,
    },
    /// Original user text, committed before routing or skill injection.
    Submitted(Submission),
    /// A local, manually chosen session name. Does not count as activity.
    SessionName {
        title: String,
    },
    /// Idempotent recovery boundary; no execution is resumed by opening it.
    Recovered {
        message: String,
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
    /// The user started this turn with `/<skill>`; the skill's text is in
    /// the `user` event that follows. `scope` is "project" or "global".
    Invoke {
        skill: String,
        scope: String,
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
    /// Live output chunk from a running tool (shell stdout/stderr). Not
    /// journaled: `ToolEnd` carries the full output.
    ToolOutput {
        id: String,
        text: String,
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
    /// One model call's accounting: `cached` of `prompt` read from the cache,
    /// `cache_write` written; `cost` and `served` (the model the response named) when the provider reports them.
    Usage {
        prompt: u64,
        completion: u64,
        cached: u64,
        #[serde(default)]
        cache_write: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cost: Option<f64>,
        served: Option<String>,
    },
    /// The model was switched mid-session (`/model`), with the limits its
    /// catalog entry (or the defaults) gave it. The head row has the first.
    Model {
        model: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<String>,
        context_window: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_tokens: Option<u32>,
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
        #[serde(default)]
        summary_cached: u64,
        /// What the summary cost, provider-reported or priced from the catalog.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        summary_cost: Option<f64>,
        /// Why a summary was planned but not written (the cut fell back to eliding).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fallback: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cut: Option<payload::Cut>,
    },
    /// The `optchat` tree changed (ADR 0043): written leaves and merges, and
    /// the view when its shape moved. Replay rebuilds from these.
    Memory {
        #[serde(default)]
        leaves: Vec<MemoryNode>,
        #[serde(default)]
        merges: Vec<MemoryNode>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        view: Option<Vec<(u32, u64)>>,
        #[serde(default)]
        summary_prompt: u64,
        #[serde(default)]
        summary_completion: u64,
        #[serde(default)]
        ms: u64,
    },
    TurnEnd {
        steps: u32,
        prompt: u64,
        completion: u64,
        ms: u64,
        interrupted: bool,
        #[serde(default)]
        cached: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cost: Option<f64>,
    },
    Error {
        message: String,
    },
    /// Something went wrong or odd that the run recovered from or never
    /// surfaced: `class` is the subsystem (`provider`), `kind` the condition
    /// (`retry`, `bad_chunk`, ...), `count` how many times in one model call.
    /// Journal-only in the UI. A new category adds `Kind` constants, not a
    /// variant (`journal::notes`).
    Note {
        class: String,
        kind: String,
        detail: String,
        count: u32,
    },
    Learn {
        status: String,
    },
    /// A delegated ACP agent asked for permission (`session/request_permission`)
    /// and the parent answered by the definition's `on_permission`. `decision`
    /// is "allow", "reject" or "cancelled" (no option of the wanted kind).
    /// Only journals from before delegation was removed (#294) have these.
    Permission {
        tool_call_id: String,
        title: String,
        /// The kinds the child offered (`allow_once`, `reject_once`, ...).
        options: Vec<String>,
        decision: String,
        /// Who decided: "policy" (`on_permission`), "config" (`allow_kinds`),
        /// or the user's answer ("once", "session", "always", "cancelled").
        #[serde(default, skip_serializing_if = "String::is_empty")]
        scope: String,
    },
    /// The user decided whether a tool may go past the sandbox. Not recorded
    /// for headless runs, which only ever say no. `decision` is "once",
    /// "session", "always", "deny" or "cancelled"; `paths` are what was granted
    /// (the folder, if the user chose that). `agent` named a delegated agent
    /// before delegation was removed (#294); it is empty in new rows.
    Access {
        #[serde(default, skip_serializing_if = "String::is_empty")]
        agent: String,
        tool: String,
        /// "read" or "write".
        access: String,
        paths: Vec<String>,
        decision: String,
    },
    /// A decision about one frozen host-authenticated developer operation.
    Operation {
        operation: String,
        scope: String,
        detail: String,
        decision: String,
    },
    /// A background job started (ADR 0026, `jobs`).
    JobStart(payload::Started),
    /// A background job ended, however it did; only some endings wake the agent.
    JobEnd(payload::Info),
    /// The model was given these endings (`payload::Delivered`).
    JobsDelivered(payload::Delivered),
    /// A boundary decision Deck pushed over ACP (either boundary method), put on
    /// the next model request. It informs; it grants nothing, and the agent
    /// cannot answer it with a decision.
    DeckBoundary {
        /// The approval ids this note consumed.
        ids: Vec<String>,
        /// What the model was sent, verbatim.
        message: Message,
    },
    /// An answer with no tool call that was not an answer (`why`: `length`,
    /// `text_tool_call`, `empty`), and what the model was sent to go on.
    Nudge {
        why: String,
        message: Message,
    },
    /// A wake event arrived for this session (ADR 0079).
    WakeReceived(payload::WakeReceived),
    /// One or more wake events reached the model at a turn boundary (ADR 0079).
    WakesDelivered(payload::WakesDelivered),
}

mod output;
pub mod payload;
mod projection;
mod submission;
pub use {output::OutputKind, payload::SessionOrigin, projection::MemoryNode, submission::Submission};
