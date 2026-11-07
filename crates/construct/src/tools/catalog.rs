//! The session's tool catalog: registration, routing groups and dispatch.
//! A disabled tool is absent from every lookup, including lazy MCP discovery.

use super::*;
use crate::config::ToolsConfig;
use anyhow::bail;
use std::collections::{BTreeSet, HashSet};
use std::sync::{Arc, RwLock};

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
        Self::build_with(cfg, mcp, false, false)
    }

    /// `build` for a process that also has a Deck boundary endpoint; the three
    /// boundary tools exist only there. `github` is Deck's GitHub gateway: it
    /// switches the `developer` tool to publish through it.
    pub fn build_with(
        cfg: &ToolsConfig,
        mcp: Option<Arc<crate::mcp::Manager>>,
        deck: bool,
        github: bool,
    ) -> Result<Catalog> {
        let mut tools: Vec<Arc<dyn Tool>> = vec![Arc::new(shell::Shell::new(&cfg.shell))];
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
        tools.push(Arc::new(web::WebSearch::new(&cfg.web)?));
        tools.push(Arc::new(web::WebFetch));
        tools.push(Arc::new(memory::Memory));
        tools.push(Arc::new(skill::SkillView));
        tools.push(Arc::new(skill::SkillManage));
        tools.push(Arc::new(history::RecallResult));
        tools.push(Arc::new(history::JournalQuery));
        tools.push(Arc::new(job::JobTool));
        tools.push(Arc::new(developer::Developer::new(github)));
        if deck {
            tools.push(Arc::new(boundary::Wait));
            tools.push(Arc::new(boundary::Status));
            tools.push(Arc::new(boundary::Withdraw));
        }
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
        #[expect(clippy::unwrap_used, reason = "poisoned lock: a panic elsewhere")]
        let mut tools = self.tools.write().unwrap();
        for t in listed {
            let t = mcp::McpTool::new(m, server, t);
            if !self.disabled.iter().any(|d| d == t.name()) && !tools.iter().any(|x| x.name() == t.name()) {
                tools.push(Arc::new(t));
            }
        }
        Ok(())
    }

    #[expect(clippy::unwrap_used, reason = "poisoned lock: a panic elsewhere")]
    fn tools(&self) -> std::sync::RwLockReadGuard<'_, Vec<Arc<dyn Tool>>> {
        self.tools.read().unwrap()
    }

    /// Each tool and its group: what a delegation definition is checked against.
    pub fn table(&self) -> Vec<(String, Group)> {
        self.tools().iter().map(|t| (t.name().to_string(), t.group())).collect()
    }

    /// Add a tool built after the catalog (one that needs to know the rest).
    #[expect(clippy::unwrap_used, reason = "poisoned lock: a panic elsewhere")]
    pub fn add(&self, tool: Arc<dyn Tool>) {
        self.tools.write().unwrap().push(tool);
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
            (Group::Developer, "approved host-authenticated GitHub issues, git commits, pushes and pull requests"),
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
