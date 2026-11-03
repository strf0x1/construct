//! The tool surface. Every tool is one `impl Tool` that owns its name,
//! schema, group, execution and display hints; the `Catalog` is the set a
//! session has, built from `[tools]` config, and is the only thing the agent,
//! the learning pass and the UI ask about tools. A tool that is not in the
//! catalog does not exist.
//!
//! Tools are grouped; the core group is always present and the others are
//! switched on per turn by the router, so the model never has to pick from a
//! huge list. Once on, a group stays on for the session: the tool list then
//! only ever grows, which keeps provider prefix caches warm.

pub mod access;
pub mod boundary;
pub mod call;
mod cap;
mod catalog;
mod ctx;
pub mod delegate;
pub mod developer;
pub mod edit;
pub mod fs;
pub mod glob;
pub mod grep;
mod group;
pub mod history;
pub mod job;
pub mod mcp;
pub mod memory;
pub mod read;
pub mod shell;
mod shell_job;
pub mod skill;
pub mod web;
pub mod write;

use anyhow::Result;
pub use call::Call;
pub use cap::{OUTPUT_MAX, cap_output};
pub use catalog::Catalog;
pub use ctx::Ctx;
pub use group::Group;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// The switch the model uses to turn on a group the router missed. The agent
/// answers it itself; it is not a catalog tool.
pub const ENABLE_TOOLS: &str = "enable_tools";

/// One tool: everything about it lives in its `impl`, and its wire name is
/// declared there and nowhere else.
#[async_trait::async_trait]
pub trait Tool: Send + Sync {
    /// Wire name the model calls.
    fn name(&self) -> &str;
    fn group(&self) -> Group;
    /// What the tool does, in ACP's vocabulary (`other` unless one fits).
    fn kind(&self) -> ToolKind {
        ToolKind::Other
    }
    /// The OpenAI function definition.
    fn definition(&self) -> Value;
    /// Changes nothing, so it may run beside other read-only calls.
    fn readonly(&self) -> bool {
        false
    }
    /// How the UI should show a call.
    fn display(&self) -> Display {
        Display::default()
    }
    async fn call(&self, ctx: &Ctx, args: Value) -> Result<Output, ToolError>;
    /// `call`, for a tool that runs work of its own and streams that work's
    /// events up through `call`. Most tools ignore it.
    async fn call_with(&self, ctx: &Ctx, _call: &Call, args: Value) -> Result<Output, ToolError> {
        self.call(ctx, args).await
    }
}

/// Categories of tool call as the Agent Client Protocol defines them: a fixed
/// list a client uses to pick an icon, which is why this is not the tool's
/// name. Variants are added when a tool needs one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolKind {
    Read,
    Edit,
    Search,
    Execute,
    Fetch,
    #[default]
    Other,
}

/// Presentation hints, so the UI never matches on tool names.
#[derive(Debug, Clone, Copy)]
pub struct Display {
    /// Argument shown on the call line; empty shows the raw arguments.
    pub primary_arg: &'static str,
    /// Lines of output shown under a finished call (0 = collapse to a count).
    pub preview_lines: usize,
}

impl Default for Display {
    fn default() -> Self {
        Self { primary_arg: "", preview_lines: 2 }
    }
}

/// How the UI renders a result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputKind {
    #[default]
    Plain,
    /// A one-line summary, then a unified diff.
    Diff,
    /// Only the end matters (command output).
    Tail,
}

pub struct Output {
    /// What the model receives.
    pub text: String,
    pub kind: OutputKind,
    /// Machine-readable result for logs and scorers.
    pub data: Option<Value>,
}

impl Output {
    pub fn plain(text: impl Into<String>) -> Self {
        Self { text: text.into(), kind: OutputKind::Plain, data: None }
    }
    pub fn with_kind(mut self, kind: OutputKind) -> Self {
        self.kind = kind;
        self
    }
    pub fn with_data(mut self, data: Value) -> Self {
        self.data = Some(data);
        self
    }
    /// False when the result says the call changed nothing (a listing).
    pub fn changed(&self) -> bool {
        self.data.as_ref().and_then(|d| d.get("changed")).and_then(Value::as_bool).unwrap_or(true)
    }
}

#[derive(Debug, Clone)]
pub struct ToolError {
    /// Prose for the model.
    pub message: String,
    /// Stable code for scorers ("not_found", "ambiguous", "sandbox", …).
    pub code: &'static str,
    pub kind: OutputKind,
    /// Machine-readable detail for logs and scorers, as on `Output`.
    pub data: Option<Value>,
}

impl ToolError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self { message: message.into(), code, kind: OutputKind::Plain, data: None }
    }
    pub fn with_data(mut self, data: Value) -> Self {
        self.data = Some(data);
        self
    }
    pub fn sandbox(message: String) -> Self {
        Self::new("sandbox", message)
    }
}

impl From<String> for ToolError {
    fn from(message: String) -> Self {
        Self::new("error", message)
    }
}

impl From<&str> for ToolError {
    fn from(message: &str) -> Self {
        Self::new("error", message)
    }
}

impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

fn def(name: &str, description: &str, params: Value) -> Value {
    json!({ "type": "function", "function": { "name": name, "description": description, "parameters": params } })
}

fn obj(props: Value, required: &[&str]) -> Value {
    json!({ "type": "object", "properties": props, "required": required })
}

fn arg<'a>(a: &'a Value, k: &str) -> Option<&'a str> {
    a.get(k).and_then(Value::as_str)
}

fn num(a: &Value, k: &str) -> Option<usize> {
    a.get(k).and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse().ok()))).map(|n| n as usize)
}

/// Filesystem walks are synchronous; keep them off the async workers.
fn blocking(ctx: &Ctx, f: impl FnOnce() -> Result<String, ToolError>) -> Result<Output, ToolError> {
    // The walk borrows ctx, so use block_in_place rather than spawn_blocking.
    let r = tokio::task::block_in_place(f)?;
    Ok(Output::plain(cap_output(ctx, "out", &r)))
}
