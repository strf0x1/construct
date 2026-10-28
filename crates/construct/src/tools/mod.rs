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
pub mod call;
pub mod delegate;
pub mod edit;
pub mod fs;
pub mod glob;
pub mod grep;
pub mod history;
pub mod mcp;
pub mod memory;
pub mod read;
pub mod shell;
pub mod skill;
pub mod web;
pub mod write;

use crate::config::{Paths, ToolsConfig};
use crate::skills::Library;
use anyhow::{Result, bail};
pub use call::Call;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use tokio::sync::watch;

pub const OUTPUT_MAX: usize = 30_000;

/// The switch the model uses to turn on a group the router missed. The agent
/// answers it itself; it is not a catalog tool.
pub const ENABLE_TOOLS: &str = "enable_tools";

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Group {
    Core,
    Web,
    Memory,
    Skills,
    SkillWrite,
    History,
    Mcp(String),
}

impl Group {
    pub fn label(&self) -> String {
        match self {
            Group::Core => "core".into(),
            Group::Web => "web".into(),
            Group::Memory => "memory".into(),
            Group::Skills => "skills".into(),
            Group::SkillWrite => "skill-write".into(),
            Group::History => "history".into(),
            Group::Mcp(s) => format!("mcp:{s}"),
        }
    }

    /// The inverse of `label` for the groups the model may ask for.
    pub fn parse(label: &str) -> Option<Group> {
        Some(match label {
            "web" => Group::Web,
            "memory" => Group::Memory,
            "history" => Group::History,
            "skills" => Group::Skills,
            "skill-write" => Group::SkillWrite,
            l => Group::Mcp(l.strip_prefix("mcp:")?.to_string()),
        })
    }
}

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

pub struct Ctx {
    pub cwd: PathBuf,
    pub paths: Paths,
    pub http: reqwest::Client,
    pub cancel: watch::Receiver<bool>,
    pub memory: crate::memory::Store,
    pub skills: Library,
    /// The current repo's kit skills, when construct runs inside a repo.
    pub project_skills: Option<Library>,
    pub mcp: Option<Arc<crate::mcp::Manager>>,
    pub sandbox: crate::sandbox::Policy,
    /// Who may be asked, and what has been approved (`permission`).
    pub perms: Arc<crate::permission::Perms>,
    read_files: Mutex<HashSet<PathBuf>>,
    /// Shell calls the sandbox blocked this turn (`shell::STOP_AFTER`).
    shell_blocked: std::sync::atomic::AtomicUsize,
}

impl Ctx {
    pub fn new(
        cwd: PathBuf,
        paths: Paths,
        memory: crate::memory::Store,
        cancel: watch::Receiver<bool>,
        mcp: Option<Arc<crate::mcp::Manager>>,
        sandbox: crate::sandbox::Policy,
    ) -> Self {
        let http = reqwest::Client::builder()
            .user_agent(concat!("construct/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(std::time::Duration::from_secs(15))
            .build()
            .expect("http client");
        let skills = Library::new(&paths);
        let project_skills = memory.project.as_ref().map(|p| Library::at(p.skills_dir()));
        let perms = Arc::new(crate::permission::Perms::new(&paths, sandbox.clone()));
        Self {
            cwd,
            paths,
            http,
            cancel,
            memory,
            skills,
            project_skills,
            mcp,
            sandbox,
            perms,
            read_files: Mutex::new(HashSet::new()),
            shell_blocked: Default::default(),
        }
    }
    /// The project kit (when there is one) and then the global library: the
    /// order a name is looked up in, so a kit skill hides a global one.
    pub fn skill_libs(&self) -> Vec<&Library> {
        self.project_skills.iter().chain(std::iter::once(&self.skills)).collect()
    }
    /// The library that holds `name`, kit first.
    pub fn skill_lib(&self, name: &str) -> Option<&Library> {
        self.skill_libs().into_iter().find(|l| l.get(name).is_some())
    }
    /// Every skill the model or the user can reach, read from disk now.
    pub fn all_skills(&self) -> Vec<crate::skills::Skill> {
        crate::skills::merged(&self.skill_libs())
    }
    /// A new user turn: per-turn counts start over.
    pub fn turn_began(&self) {
        self.shell_blocked.store(0, std::sync::atomic::Ordering::Relaxed);
    }
    /// Shell calls blocked so far this turn.
    pub fn shell_blocks(&self) -> usize {
        self.shell_blocked.load(std::sync::atomic::Ordering::Relaxed)
    }
    /// Count one more blocked shell call; the new total.
    pub fn shell_blocked(&self) -> usize {
        self.shell_blocked.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1
    }
    pub fn mark_read(&self, p: &Path) {
        self.read_files.lock().unwrap().insert(p.to_path_buf());
    }
    pub fn was_read(&self, p: &Path) -> bool {
        self.read_files.lock().unwrap().contains(p)
    }
}

/// The tools this session has. Definition order in requests is the order
/// tools are pushed in `build` (then MCP tools in discovery order), grouped
/// by `Group`'s order.
pub struct Catalog {
    /// Grows only when an MCP server starts (`start_mcp`).
    tools: RwLock<Vec<Arc<dyn Tool>>>,
    mcp: Option<Arc<crate::mcp::Manager>>,
    /// Names removed by `[tools] disabled`, kept to filter MCP tools, which
    /// are only known once their server starts.
    disabled: Vec<String>,
}

impl Catalog {
    /// The one place implementations are chosen.
    pub fn build(cfg: &ToolsConfig, mcp: Option<Arc<crate::mcp::Manager>>) -> Result<Catalog> {
        let mut tools: Vec<Arc<dyn Tool>> = vec![Arc::new(shell::Shell)];
        match cfg.editor.as_str() {
            "replace" => {
                tools.push(Arc::new(read::Read));
                tools.push(Arc::new(write::Write));
                tools.push(Arc::new(edit::Edit));
            }
            "hashline" => {
                tools.push(Arc::new(edit::hashline::HashlineRead::new(&cfg.hashline)));
                tools.push(Arc::new(write::Write));
                tools.push(Arc::new(edit::hashline::HashlineEdit::new(&cfg.hashline)));
            }
            other => bail!("tools.editor: unknown editor {other:?} (expected \"replace\" or \"hashline\")"),
        }
        if cfg.search_name.is_empty()
            || !cfg.search_name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            bail!("tools.search_name: {:?} is not a usable tool name", cfg.search_name);
        }
        tools.push(Arc::new(grep::Grep::named(&cfg.search_name)));
        tools.push(Arc::new(glob::Glob));
        tools.push(Arc::new(web::WebSearch));
        tools.push(Arc::new(web::WebFetch));
        tools.push(Arc::new(memory::Memory));
        tools.push(Arc::new(skill::SkillView));
        tools.push(Arc::new(skill::SkillManage));
        tools.push(Arc::new(history::RecallResult));
        tools.push(Arc::new(history::JournalQuery));
        let mut seen = HashSet::new();
        if let Some(t) = tools.iter().find(|t| !seen.insert(t.name().to_string())) {
            bail!("tools.search_name: {:?} is already a tool", t.name());
        }
        for d in &cfg.disabled {
            if crate::mcp::server_of(d).is_none() && !tools.iter().any(|t| t.name() == d) {
                bail!("tools.disabled: unknown tool {d:?}");
            }
        }
        tools.retain(|t| !cfg.disabled.iter().any(|d| d == t.name()));
        Ok(Catalog { tools: RwLock::new(tools), mcp, disabled: cfg.disabled.clone() })
    }

    /// Start an MCP server if needed and add the tools it lists, minus
    /// `[tools] disabled`. Calling it again for a started server adds nothing.
    pub async fn start_mcp(&self, server: &str) -> Result<()> {
        let Some(m) = &self.mcp else { bail!("no MCP server {server}") };
        let listed = m.ensure(server).await?;
        let mut tools = self.tools.write().unwrap(); // a poisoned lock is a panic elsewhere
        for t in listed {
            let t = mcp::McpTool::new(m, server, t);
            if !self.disabled.iter().any(|d| d == t.name()) && !tools.iter().any(|x| x.name() == t.name()) {
                tools.push(Arc::new(t));
            }
        }
        Ok(())
    }

    fn tools(&self) -> std::sync::RwLockReadGuard<'_, Vec<Arc<dyn Tool>>> {
        self.tools.read().unwrap() // a poisoned lock is a panic elsewhere
    }

    /// Each tool and its group: what a delegation definition is checked against.
    pub fn table(&self) -> Vec<(String, Group)> {
        self.tools().iter().map(|t| (t.name().to_string(), t.group())).collect()
    }

    /// Add a tool built after the catalog (one that needs to know the rest).
    pub fn add(&self, tool: Arc<dyn Tool>) {
        self.tools.write().unwrap().push(tool); // a poisoned lock is a panic elsewhere
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools().iter().find(|t| t.name() == name).cloned()
    }

    /// The kind of the named tool; `other` for one the catalog doesn't have.
    pub fn kind_of(&self, name: &str) -> ToolKind {
        self.get(name).map(|t| t.kind()).unwrap_or_default()
    }

    pub fn names(&self) -> Vec<String> {
        self.tools().iter().map(|t| t.name().to_string()).collect()
    }

    /// Tool definitions for the enabled groups, in a stable order.
    pub fn definitions<'a>(&self, enabled: impl IntoIterator<Item = &'a Group>) -> Vec<Value> {
        let enabled: BTreeSet<&Group> = enabled.into_iter().collect();
        let tools = self.tools();
        let mut out = Vec::new();
        for g in enabled {
            out.extend(tools.iter().filter(|t| t.group() == *g).map(|t| t.definition()));
        }
        out
    }

    /// The group that owns `name`, so a call to a tool the model wasn't shown
    /// can switch its group on. None for anything not in the catalog.
    pub fn group_of(&self, name: &str) -> Option<Group> {
        if let Some(t) = self.get(name) {
            return Some(t.group());
        }
        if self.disabled.iter().any(|d| d == name) {
            return None;
        }
        // A server's tools join the catalog when it starts; naming one of
        // them is how the model asks for that.
        Some(Group::Mcp(crate::mcp::server_of(name)?.to_string()))
    }

    pub fn readonly(&self, name: &str) -> bool {
        self.get(name).is_some_and(|t| t.readonly())
    }

    pub fn display(&self, name: &str) -> Display {
        match self.get(name) {
            Some(t) => t.display(),
            None if name == ENABLE_TOOLS => Display { primary_arg: "groups", ..Display::default() },
            None => Display::default(),
        }
    }

    /// Groups not yet on, with a short description for `enable_tools`.
    pub fn offerable(&self, enabled: &BTreeSet<Group>) -> Vec<(Group, String)> {
        let mut out = vec![];
        let base = [
            (Group::Web, ""),
            (Group::Memory, "durable memory of the user and machine"),
            (Group::History, "search past sessions"),
            (Group::Skills, "load saved procedures"),
            (Group::SkillWrite, "write the skill library"),
        ];
        for (g, d) in base {
            let names: Vec<String> =
                self.tools().iter().filter(|t| t.group() == g).map(|t| t.name().to_string()).collect();
            if names.is_empty() || enabled.contains(&g) {
                continue;
            }
            // The web pair is described by its tools' names.
            let d = if d.is_empty() { names.join(", ") } else { d.to_string() };
            out.push((g, d));
        }
        if let Some(m) = &self.mcp {
            for (name, s) in m.servers() {
                let g = Group::Mcp(name.clone());
                if !enabled.contains(&g) {
                    out.push((g, crate::llm::truncate(&s.description, 80)));
                }
            }
        }
        out
    }

    /// Run a call by name. Unknown and disabled tools are both "unknown tool".
    pub async fn call(&self, ctx: &Ctx, name: &str, args: &str) -> Result<Output, ToolError> {
        self.call_with(ctx, &Call::detached(""), name, args).await
    }

    /// `call` for a call the agent is running: `call` carries its id and the
    /// session's event stream.
    pub async fn call_with(&self, ctx: &Ctx, call: &Call, name: &str, args: &str) -> Result<Output, ToolError> {
        let a: Value = if args.trim().is_empty() {
            json!({})
        } else {
            serde_json::from_str(args).map_err(|e| {
                ToolError::new("bad_json", format!("arguments are not valid JSON ({e}); resend the call"))
            })?
        };
        if let Some(t) = self.get(name) {
            return t.call_with(ctx, call, a).await;
        }
        Err(ToolError::new("unknown_tool", format!("unknown tool {name}")))
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

/// Keep tool output inside the context budget. Long output keeps its head and
/// tail; the whole thing is spilled to a file the model can read or grep.
pub fn cap_output(ctx: &Ctx, kind: &str, text: &str) -> String {
    if text.len() <= OUTPUT_MAX {
        return text.to_string();
    }
    let dir = ctx.paths.spill_dir();
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join(format!("{kind}-{}-{}.txt", std::process::id(), crate::skills::now_nanos()));
    let saved = std::fs::write(&path, text).is_ok();
    let head = crate::llm::truncate(text, OUTPUT_MAX / 2);
    let mut tail_start = text.len() - OUTPUT_MAX / 3;
    while !text.is_char_boundary(tail_start) {
        tail_start += 1;
    }
    let lines = text.lines().count();
    format!(
        "{head}\n\n[… {} bytes, {lines} lines total{} …]\n\n{}",
        text.len(),
        if saved { format!("; full output in {}", path.display()) } else { String::new() },
        &text[tail_start..]
    )
}
