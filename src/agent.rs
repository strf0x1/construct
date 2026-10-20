//! The agent loop. One user turn: route, stream the model, run the tools it
//! asks for (read-only ones in parallel), repeat until it answers in text.
//! Everything the UI shows arrives as an `Event`, so the TUI, the headless
//! runner and the tests all observe the same thing.

use crate::config::{Config, Paths};
use crate::llm::{self, Delta, Message, Usage};
use crate::router::{self, Route};
use crate::session;
use crate::tools::{self, Ctx, Group};
use anyhow::Result;
use futures::future::join_all;
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{mpsc, watch};

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    TurnStart,
    Route { groups: Vec<String>, skills: Vec<String>, source: String, ms: u64, note: String },
    /// The model's response stream opened.
    Opened,
    Text { text: String },
    Reasoning { text: String },
    /// The model has started emitting a tool call (arguments still streaming).
    ToolPending { name: String },
    ToolStart { id: String, name: String, args: String },
    ToolEnd { id: String, name: String, ok: bool, output: String, ms: u64 },
    Usage { prompt: u64, completion: u64, cached: u64 },
    Compacted { before: usize, after: usize },
    TurnEnd { steps: u32, prompt: u64, completion: u64, ms: u64, interrupted: bool },
    Error { message: String },
    Learn { status: String },
}

pub type Events = mpsc::UnboundedSender<Event>;

pub struct Agent {
    pub cfg: Config,
    pub paths: Paths,
    pub client: llm::Client,
    pub ctx: Arc<Ctx>,
    pub messages: Vec<Message>,
    pub groups: BTreeSet<Group>,
    pub log: session::Log,
    pub usage: Usage,
    pub last_prompt_tokens: u64,
    pub turns_since_memory: u32,
    pub tool_calls_since_skill: u32,
    pub user_turns: u32,
    pub tool_calls: u32,
    cancel_tx: watch::Sender<bool>,
}

/// Optional groups the model can ask for itself when the router missed one.
const ENABLE_TOOLS: &str = "enable_tools";

impl Agent {
    pub fn new(cfg: Config, paths: Paths, cwd: std::path::PathBuf, resume: Option<String>) -> Result<Self> {
        let key = crate::secrets::api_key(&cfg.api_key_secret);
        let client = llm::Client::new(&cfg.base_url, key, cfg.headers.clone())?;
        let memory = crate::memory::Store::new(&paths, &cfg.memory).with_project(&paths, &cwd);
        let (cancel_tx, cancel_rx) = watch::channel(false);
        let mcp = if cfg.mcp.is_empty() { None } else { Some(Arc::new(crate::mcp::Manager::new(cfg.mcp.clone()))) };
        let cwd_s = cwd.display().to_string();
        let (log, mut messages) = match resume {
            Some(id) => session::Log::open(&paths, &id)?,
            None => (session::Log::create(&paths, &cwd_s, &cfg.model)?, vec![]),
        };
        let system = crate::prompt::build(&cwd, &memory);
        match messages.first() {
            Some(m) if m.role == "system" => messages[0] = Message::system(system),
            _ => messages.insert(0, Message::system(system)),
        }
        let sandbox = crate::sandbox::Policy::new(&cfg.sandbox, &cwd, &paths);
        let ctx = Arc::new(Ctx::new(cwd, paths.clone(), memory, cancel_rx, mcp, sandbox));
        let mut groups = BTreeSet::new();
        groups.insert(Group::Core);
        let mut agent = Self {
            cfg,
            paths,
            client,
            ctx,
            messages,
            groups,
            log,
            usage: Usage::default(),
            last_prompt_tokens: 0,
            turns_since_memory: 0,
            tool_calls_since_skill: 0,
            user_turns: 0,
            tool_calls: 0,
            cancel_tx,
        };
        if agent.messages.len() == 1 {
            let sys = agent.messages[0].clone();
            agent.log.message(&sys);
        }
        Ok(agent)
    }

    /// Pre-connect to the model endpoint and to Jev in the background.
    pub fn warm(&self) {
        self.client.warm();
        router::warm(&self.cfg.router, &self.ctx.http);
    }

    /// A handle the UI can use to interrupt the running turn.
    pub fn canceller(&self) -> watch::Sender<bool> {
        self.cancel_tx.clone()
    }

    fn cancelled(&self) -> bool {
        *self.cancel_tx.borrow()
    }

    fn tool_defs(&self) -> Vec<Value> {
        let groups: Vec<Group> = self.groups.iter().cloned().collect();
        let mut defs = tools::definitions(&groups, &self.ctx);
        let offer = self.offerable();
        if !offer.is_empty() {
            let desc = format!(
                "Switch on more tools for this session when you need them: {}.",
                offer.iter().map(|(k, d)| format!("{k} ({d})")).collect::<Vec<_>>().join("; ")
            );
            defs.push(json!({
                "type": "function",
                "function": {
                    "name": ENABLE_TOOLS,
                    "description": desc,
                    "parameters": {
                        "type": "object",
                        "properties": { "groups": { "type": "array", "items": { "type": "string", "enum": offer.iter().map(|(k, _)| k.clone()).collect::<Vec<_>>() } } },
                        "required": ["groups"]
                    }
                }
            }));
        }
        defs
    }

    /// Groups not yet on, with a short description for `enable_tools`.
    fn offerable(&self) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = vec![];
        let base = [
            (Group::Web, "web_search, web_fetch"),
            (Group::Memory, "durable memory of the user and machine"),
            (Group::History, "search past sessions"),
            (Group::Skills, "load saved procedures"),
            (Group::SkillWrite, "write the skill library"),
        ];
        for (g, d) in base {
            if !self.groups.contains(&g) {
                out.push((g.label(), d.to_string()));
            }
        }
        if let Some(m) = &self.ctx.mcp {
            for (name, s) in m.servers() {
                let g = Group::Mcp(name.clone());
                if !self.groups.contains(&g) {
                    out.push((g.label(), crate::llm::truncate(&s.description, 80)));
                }
            }
        }
        out
    }

    fn group_for_tool(name: &str) -> Option<Group> {
        Some(match name {
            "web_search" | "web_fetch" => Group::Web,
            "memory" => Group::Memory,
            "session_search" => Group::History,
            "skill" => Group::Skills,
            "skill_manage" => Group::SkillWrite,
            n if n.starts_with("mcp__") => Group::Mcp(n[5..].split("__").next()?.to_string()),
            _ => return None,
        })
    }

    fn parse_group(label: &str) -> Option<Group> {
        Some(match label {
            "web" => Group::Web,
            "memory" => Group::Memory,
            "history" => Group::History,
            "skills" => Group::Skills,
            "skill-write" => Group::SkillWrite,
            l => Group::Mcp(l.strip_prefix("mcp:")?.to_string()),
        })
    }

    async fn enable(&mut self, groups: impl IntoIterator<Item = Group>, tx: &Events) {
        for g in groups {
            if let Group::Mcp(name) = &g {
                if let Some(m) = &self.ctx.mcp {
                    if let Err(e) = m.ensure(name).await {
                        let _ = tx.send(Event::Error { message: format!("MCP {name}: {e:#}") });
                        continue;
                    }
                }
            }
            self.groups.insert(g);
        }
    }

    pub async fn route(&self, text: &str) -> Route {
        let previous = self
            .messages
            .iter()
            .rev()
            .find(|m| m.role == "assistant" && m.content.is_some())
            .and_then(|m| m.content.as_deref())
            .unwrap_or("");
        let skills = self.ctx.skills.list();
        let mcp: BTreeMap<String, String> = self
            .ctx
            .mcp
            .as_ref()
            .map(|m| m.servers().iter().map(|(k, v)| (k.clone(), v.description.clone())).collect())
            .unwrap_or_default();
        router::route(&self.cfg.router, &self.ctx.http, router::Input { request: text, previous, skills: &skills, mcp: &mcp }).await
    }

    pub async fn turn(&mut self, text: &str, tx: &Events) -> Result<()> {
        let started = Instant::now();
        self.cancel_tx.send_replace(false);
        let _ = tx.send(Event::TurnStart);
        let route = self.route(text).await;
        let _ = tx.send(Event::Route {
            groups: route.groups.iter().map(Group::label).collect(),
            skills: route.skills.iter().map(|(n, p)| format!("{n} {p:.2}")).collect(),
            source: route.source.clone(),
            ms: route.ms,
            note: route.note.clone(),
        });
        self.log.event("route", json!({ "groups": route.groups.iter().map(Group::label).collect::<Vec<_>>(), "skills": route.skills, "source": route.source, "ms": route.ms, "note": route.note }));
        self.enable(route.groups.clone(), tx).await;

        for (name, _) in &route.skills {
            self.ctx.skills.suggested(name);
        }
        let mut content = text.to_string();
        if let Some(r) = crate::prompt::skill_reminder(&route.skills, &self.ctx.skills) {
            content.push_str("\n\n");
            content.push_str(&r);
        }
        let user = Message::user(content);
        self.log.message(&user);
        self.messages.push(user);
        self.user_turns += 1;
        self.turns_since_memory += 1;

        let mut steps = 0u32;
        let mut turn_usage = Usage::default();
        let mut interrupted = false;
        loop {
            if self.cancelled() {
                interrupted = true;
                break;
            }
            if steps >= self.cfg.max_steps {
                let _ = tx.send(Event::Error { message: format!("stopped after {steps} steps (max_steps)") });
                break;
            }
            self.maybe_compact(tx).await;
            steps += 1;
            let defs = self.tool_defs();
            let completion = match self.step(&defs, tx).await {
                Ok(Some(c)) => c,
                Ok(None) => {
                    interrupted = true;
                    break;
                }
                Err(e) => {
                    let _ = tx.send(Event::Error { message: format!("{e:#}") });
                    break;
                }
            };
            turn_usage.add(&completion.usage);
            self.usage.add(&completion.usage);
            self.last_prompt_tokens = completion.usage.prompt_tokens;
            self.log.usage(&completion.usage, &self.cfg.model);
            let _ = tx.send(Event::Usage {
                prompt: completion.usage.prompt_tokens,
                completion: completion.usage.completion_tokens,
                cached: completion.usage.cached_tokens,
            });
            let msg = completion.message;
            self.log.message(&msg);
            self.messages.push(msg.clone());
            if msg.tool_calls.is_empty() {
                if completion.finish_reason == "length" {
                    let _ = tx.send(Event::Error { message: "the model hit its output limit mid-answer".into() });
                }
                break;
            }
            self.run_tools(&msg.tool_calls, tx).await;
        }
        let _ = tx.send(Event::TurnEnd {
            steps,
            prompt: turn_usage.prompt_tokens,
            completion: turn_usage.completion_tokens,
            ms: started.elapsed().as_millis() as u64,
            interrupted,
        });
        Ok(())
    }

    /// One model call. Returns None when interrupted.
    async fn step(&self, defs: &[Value], tx: &Events) -> Result<Option<llm::Completion>> {
        let (dtx, mut drx) = mpsc::unbounded_channel::<Delta>();
        let req = llm::Request {
            model: &self.cfg.model,
            messages: &self.messages,
            tools: defs,
            max_tokens: self.cfg.max_tokens,
            temperature: self.cfg.temperature,
        };
        let fut = self.client.stream(req, Some(&dtx));
        tokio::pin!(fut);
        let mut cancel = self.ctx.cancel.clone();
        loop {
            tokio::select! {
                biased;
                d = drx.recv() => if let Some(d) = d { forward(d, tx) },
                r = &mut fut => {
                    while let Ok(d) = drx.try_recv() { forward(d, tx) }
                    return r.map(Some);
                }
                _ = cancel.changed() => if *cancel.borrow() { return Ok(None) },
            }
        }
    }

    async fn run_tools(&mut self, calls: &[llm::ToolCall], tx: &Events) {
        // Model-facing switches first: they change state, not the world.
        let mut results: Vec<Option<(bool, String)>> = vec![None; calls.len()];
        for (i, c) in calls.iter().enumerate() {
            if c.function.name == ENABLE_TOOLS {
                let v: Value = serde_json::from_str(&c.function.arguments).unwrap_or_default();
                let want: Vec<Group> = v["groups"]
                    .as_array()
                    .map(|a| a.iter().filter_map(Value::as_str).filter_map(Self::parse_group).collect())
                    .unwrap_or_default();
                let labels: Vec<String> = want.iter().map(Group::label).collect();
                self.enable(want, tx).await;
                let out = format!("enabled: {}. The new tools are available from your next step.", labels.join(", "));
                let _ = tx.send(Event::ToolStart { id: c.id.clone(), name: c.function.name.clone(), args: c.function.arguments.clone() });
                let _ = tx.send(Event::ToolEnd { id: c.id.clone(), name: c.function.name.clone(), ok: true, output: out.clone(), ms: 0 });
                results[i] = Some((true, out));
            } else if let Some(g) = Self::group_for_tool(&c.function.name) {
                // The model called a tool it wasn't shown: honour it and keep the group on.
                if !self.groups.contains(&g) {
                    self.enable([g], tx).await;
                }
            }
        }
        let parallel = calls.iter().all(|c| tools::is_readonly(&c.function.name) || c.function.name == ENABLE_TOOLS);
        let ctx = self.ctx.clone();
        let run = |c: llm::ToolCall| {
            let ctx = ctx.clone();
            let tx = tx.clone();
            async move {
                let _ = tx.send(Event::ToolStart { id: c.id.clone(), name: c.function.name.clone(), args: c.function.arguments.clone() });
                let t = Instant::now();
                let r = tools::call(&ctx, &c.function.name, &c.function.arguments).await;
                let ms = t.elapsed().as_millis() as u64;
                let (ok, out) = match r {
                    Ok(s) => (true, s),
                    Err(e) => (false, format!("Error: {e}")),
                };
                let _ = tx.send(Event::ToolEnd { id: c.id.clone(), name: c.function.name.clone(), ok, output: out.clone(), ms });
                (ok, out)
            }
        };
        let pending: Vec<(usize, llm::ToolCall)> =
            calls.iter().cloned().enumerate().filter(|(i, _)| results[*i].is_none()).collect();
        if parallel {
            let outs = join_all(pending.iter().map(|(_, c)| run(c.clone()))).await;
            for ((i, _), o) in pending.iter().zip(outs) {
                results[*i] = Some(o);
            }
        } else {
            for (i, c) in pending {
                if self.cancelled() {
                    results[i] = Some((false, "Error: interrupted by the user".into()));
                    continue;
                }
                results[i] = Some(run(c).await);
            }
        }
        for (c, r) in calls.iter().zip(results) {
            let (_, out) = r.unwrap_or((false, "Error: not run".into()));
            let m = Message::tool(&c.id, out);
            self.log.message(&m);
            self.messages.push(m);
            self.tool_calls += 1;
            self.tool_calls_since_skill += 1;
        }
    }

    /// Summarize older turns once the prompt nears the context window.
    async fn maybe_compact(&mut self, tx: &Events) {
        let window = self.cfg_context_window();
        if self.last_prompt_tokens < window * 3 / 4 {
            return;
        }
        // Keep the system prompt and the last few user turns verbatim; cut
        // only at a user message so tool calls stay paired with results.
        let user_idx: Vec<usize> = self.messages.iter().enumerate().filter(|(_, m)| m.role == "user").map(|(i, _)| i).collect();
        let Some(&cut) = user_idx.iter().rev().nth(1).or(user_idx.last()) else { return };
        if cut <= 1 {
            return;
        }
        let transcript = crate::learn::digest(&self.messages[1..cut], 120_000);
        let req = vec![
            Message::system("You compress agent transcripts. Write a dense summary another agent can continue from: the user's goals and constraints, decisions made, files touched and their current state, commands that worked, open problems, and next steps. No preamble."),
            Message::user(transcript),
        ];
        let r = self
            .client
            .complete(llm::Request { model: &self.cfg.model, messages: &req, tools: &[], max_tokens: Some(4000), temperature: None })
            .await;
        let Ok(c) = r else { return };
        let summary = c.message.content.unwrap_or_default();
        if summary.trim().is_empty() {
            return;
        }
        let before = self.messages.len();
        let tail = self.messages.split_off(cut);
        self.messages.truncate(1);
        self.messages.push(Message::user(format!("<system-reminder>\nEarlier conversation, summarized:\n{summary}\n</system-reminder>")));
        self.messages.push(Message::text("assistant", "Understood; continuing from that summary."));
        self.messages.extend(tail);
        self.last_prompt_tokens = 0;
        self.log.event("compact", json!({ "before": before, "after": self.messages.len() }));
        let _ = tx.send(Event::Compacted { before, after: self.messages.len() });
    }

    fn cfg_context_window(&self) -> u64 {
        self.cfg.context_window.max(8192) as u64
    }
}

fn forward(d: Delta, tx: &Events) {
    let _ = tx.send(match d {
        Delta::Text(t) => Event::Text { text: t },
        Delta::Reasoning(t) => Event::Reasoning { text: t },
        Delta::ToolStarted { name, .. } => Event::ToolPending { name },
        Delta::Opened => Event::Opened,
    });
}
