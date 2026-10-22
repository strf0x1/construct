//! The agent loop. One user turn: route, stream the model, run the tools it
//! asks for (read-only ones in parallel), repeat until it answers in text.
//! Everything the UI shows arrives as an `Event`, so the TUI, the headless
//! runner and the tests all observe the same thing, and every durable event
//! is journaled on the way out (`journal::Writer::emit`).

use crate::config::{Config, Paths};
use crate::journal;
use crate::llm::{self, Delta, Message, Usage};
use crate::manifest::Manifest;
use crate::router::{self, Route};
use crate::tools::{Catalog, Ctx, ENABLE_TOOLS, Group, OutputKind};
use anyhow::Result;
use futures::future::join_all;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{mpsc, watch};

pub use crate::event::{Event, Events};

pub struct Agent {
    pub cfg: Config,
    pub paths: Paths,
    pub client: llm::Client,
    pub ctx: Arc<Ctx>,
    pub catalog: Arc<Catalog>,
    pub manifest: Manifest,
    pub messages: Vec<Message>,
    pub groups: BTreeSet<Group>,
    pub journal: Arc<journal::Writer>,
    /// Journaled before there was a UI to send them to; sent at the first turn.
    unsent: Vec<Event>,
    pub usage: Usage,
    pub last_prompt_tokens: u64,
    pub turns_since_memory: u32,
    pub tool_calls_since_skill: u32,
    pub user_turns: u32,
    pub tool_calls: u32,
    cancel_tx: watch::Sender<bool>,
}

impl Agent {
    pub fn new(cfg: Config, paths: Paths, cwd: std::path::PathBuf, resume: Option<String>) -> Result<Self> {
        let key = crate::secrets::api_key(&cfg.api_key_secret);
        let client = llm::Client::new(&cfg.base_url, key, cfg.headers.clone())?;
        let memory = crate::memory::Store::new(&paths, &cfg.memory).with_project(&paths, &cwd);
        let (cancel_tx, cancel_rx) = watch::channel(false);
        let mcp = if cfg.mcp.is_empty() {
            None
        } else {
            Some(Arc::new(crate::mcp::Manager::new(cfg.mcp.clone(), &cfg.sandbox, &cwd, &paths)))
        };
        let catalog = Arc::new(Catalog::build(&cfg.tools, mcp.clone())?);
        let manifest = Manifest::new(&cfg, &catalog);
        let cwd_s = cwd.display().to_string();
        let file = journal::path(&paths, &cwd);
        let mut unsent = vec![];
        let (writer, mut messages) = match resume {
            Some(id) => journal::Writer::resume(&file, &id)?,
            None => {
                let (w, head) = journal::Writer::create(&file, |id| Event::Head {
                    id,
                    harness: "construct".into(),
                    version: env!("CARGO_PKG_VERSION").into(),
                    cwd: cwd_s.clone(),
                    model: cfg.model.clone(),
                    manifest: manifest.clone(),
                    parent_session: None,
                    parent_call_id: None,
                })?;
                unsent.push(head);
                (w, vec![])
            }
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
            catalog,
            manifest,
            messages,
            groups,
            journal: Arc::new(writer),
            unsent,
            usage: Usage::default(),
            last_prompt_tokens: 0,
            turns_since_memory: 0,
            tool_calls_since_skill: 0,
            user_turns: 0,
            tool_calls: 0,
            cancel_tx,
        };
        if agent.messages.len() == 1 {
            let ev = Event::for_message(agent.messages[0].clone());
            agent.journal.record(&ev);
            agent.unsent.push(ev);
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
        let mut defs = self.catalog.definitions(&self.groups);
        let offer: Vec<(String, String)> =
            self.catalog.offerable(&self.groups).into_iter().map(|(g, d)| (g.label(), d)).collect();
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

    async fn enable(&mut self, groups: impl IntoIterator<Item = Group>, tx: &Events) {
        for g in groups {
            if let Group::Mcp(name) = &g
                && let Err(e) = self.catalog.start_mcp(name).await
            {
                self.journal.emit(Event::Error { message: format!("MCP {name}: {e:#}") }, tx);
                continue;
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
        router::route(
            &self.cfg.router,
            &self.ctx.http,
            router::Input { request: text, previous, skills: &skills, mcp: &mcp },
        )
        .await
    }

    pub async fn turn(&mut self, text: &str, tx: &Events) -> Result<()> {
        let started = Instant::now();
        self.cancel_tx.send_replace(false);
        for ev in std::mem::take(&mut self.unsent) {
            let _ = tx.send(ev);
        }
        self.journal.emit(Event::TurnStart, tx);
        if self.user_turns == 0 {
            self.journal.emit(Event::Manifest(self.manifest.clone()), tx);
        }
        let route = self.route(text).await;
        self.journal.emit(
            Event::Route {
                groups: route.groups.iter().map(Group::label).collect(),
                skills: route.skills.iter().map(|(n, p)| format!("{n} {p:.2}")).collect(),
                source: route.source.clone(),
                ms: route.ms,
                note: route.note.clone(),
            },
            tx,
        );
        self.enable(route.groups.clone(), tx).await;

        for (name, _) in &route.skills {
            self.ctx.skills.suggested(name);
        }
        let mut content = text.to_string();
        if let Some(r) = crate::prompt::skill_reminder(&route.skills, &self.ctx.skills) {
            content.push_str("\n\n");
            content.push_str(&r);
        }
        self.push(Message::user(content), tx);
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
                self.journal.emit(Event::Error { message: format!("stopped after {steps} steps (max_steps)") }, tx);
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
                    self.journal.emit(Event::Error { message: format!("{e:#}") }, tx);
                    break;
                }
            };
            turn_usage.add(&completion.usage);
            self.usage.add(&completion.usage);
            self.last_prompt_tokens = completion.usage.prompt_tokens;
            self.journal.emit(
                Event::Usage {
                    prompt: completion.usage.prompt_tokens,
                    completion: completion.usage.completion_tokens,
                    cached: completion.usage.cached_tokens,
                },
                tx,
            );
            let calls = completion.message.tool_calls.clone();
            self.push(completion.message, tx);
            if calls.is_empty() {
                if completion.finish_reason == "length" {
                    self.journal.emit(Event::Error { message: "the model hit its output limit mid-answer".into() }, tx);
                }
                break;
            }
            self.run_tools(&calls, tx).await;
        }
        self.journal.emit(
            Event::TurnEnd {
                steps,
                prompt: turn_usage.prompt_tokens,
                completion: turn_usage.completion_tokens,
                ms: started.elapsed().as_millis() as u64,
                interrupted,
            },
            tx,
        );
        Ok(())
    }

    /// Append a message to the conversation, journaled.
    fn push(&mut self, m: Message, tx: &Events) {
        self.journal.emit(Event::for_message(m.clone()), tx);
        self.messages.push(m);
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
        let body = llm::body(&req);
        self.journal.emit(
            Event::Request {
                model: self.cfg.model.clone(),
                bytes: body.len() as u64,
                n_tools: defs.len() as u64,
                tools: defs.iter().filter_map(|d| d["function"]["name"].as_str().map(String::from)).collect(),
            },
            tx,
        );
        let fut = self.client.stream_body(body, Some(&dtx));
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
        // Each slot is the text the model gets back.
        let mut results: Vec<Option<String>> = vec![None; calls.len()];
        for (i, c) in calls.iter().enumerate() {
            if c.function.name == ENABLE_TOOLS {
                let v: Value = serde_json::from_str(&c.function.arguments).unwrap_or_default();
                let want: Vec<Group> = v["groups"]
                    .as_array()
                    .map(|a| a.iter().filter_map(Value::as_str).filter_map(Group::parse).collect())
                    .unwrap_or_default();
                let labels: Vec<String> = want.iter().map(Group::label).collect();
                self.enable(want, tx).await;
                let out = format!("enabled: {}. The new tools are available from your next step.", labels.join(", "));
                self.journal.emit(
                    Event::ToolStart {
                        id: c.id.clone(),
                        name: c.function.name.clone(),
                        args: c.function.arguments.clone(),
                    },
                    tx,
                );
                self.journal.emit(
                    Event::ToolEnd {
                        id: c.id.clone(),
                        name: c.function.name.clone(),
                        ok: true,
                        output: out.clone(),
                        ms: 0,
                        kind: OutputKind::Plain,
                        data: None,
                        code: None,
                    },
                    tx,
                );
                results[i] = Some(out);
            } else if let Some(g) = self.catalog.group_of(&c.function.name) {
                // The model called a tool it wasn't shown: honour it and keep the group on.
                if !self.groups.contains(&g) {
                    self.enable([g], tx).await;
                }
            }
        }
        let parallel = calls.iter().all(|c| self.catalog.readonly(&c.function.name) || c.function.name == ENABLE_TOOLS);
        let ctx = self.ctx.clone();
        let catalog = self.catalog.clone();
        let journal = self.journal.clone();
        let run = |c: llm::ToolCall| {
            let ctx = ctx.clone();
            let catalog = catalog.clone();
            let journal = journal.clone();
            let tx = tx.clone();
            async move {
                journal.emit(
                    Event::ToolStart {
                        id: c.id.clone(),
                        name: c.function.name.clone(),
                        args: c.function.arguments.clone(),
                    },
                    &tx,
                );
                let t = Instant::now();
                let r = catalog.call(&ctx, &c.function.name, &c.function.arguments).await;
                let ms = t.elapsed().as_millis() as u64;
                let (ok, out, kind, data, code) = match r {
                    Ok(o) => (true, o.text, o.kind, o.data, None),
                    Err(e) => (false, format!("Error: {}", e.message), e.kind, e.data, Some(e.code.to_string())),
                };
                // What scorers read back from the journal: codes and data,
                // never the prose.
                journal.emit(
                    Event::ToolEnd {
                        id: c.id.clone(),
                        name: c.function.name.clone(),
                        ok,
                        output: out.clone(),
                        ms,
                        kind,
                        data,
                        code,
                    },
                    &tx,
                );
                out
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
                    results[i] = Some("Error: interrupted by the user".into());
                    continue;
                }
                results[i] = Some(run(c).await);
            }
        }
        for (c, r) in calls.iter().zip(results) {
            let out = r.unwrap_or_else(|| "Error: not run".into());
            self.push(Message::tool(&c.id, out), tx);
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
        let user_idx: Vec<usize> =
            self.messages.iter().enumerate().filter(|(_, m)| m.role == "user").map(|(i, _)| i).collect();
        let Some(&cut) = user_idx.iter().rev().nth(1).or(user_idx.last()) else { return };
        if cut <= 1 {
            return;
        }
        let transcript = crate::learn::digest(&self.messages[1..cut], 120_000);
        let req = vec![
            Message::system(
                "You compress agent transcripts. Write a dense summary another agent can continue from: the user's goals and constraints, decisions made, files touched and their current state, commands that worked, open problems, and next steps. No preamble.",
            ),
            Message::user(transcript),
        ];
        let r = self
            .client
            .complete(llm::Request {
                model: &self.cfg.model,
                messages: &req,
                tools: &[],
                max_tokens: Some(4000),
                temperature: None,
            })
            .await;
        let Ok(c) = r else { return };
        let summary = c.message.content.unwrap_or_default();
        if summary.trim().is_empty() {
            return;
        }
        let before = self.messages.len();
        let tail = self.messages.split_off(cut);
        self.messages.truncate(1);
        self.messages.push(Message::user(format!(
            "<system-reminder>\nEarlier conversation, summarized:\n{summary}\n</system-reminder>"
        )));
        self.messages.push(Message::text("assistant", "Understood; continuing from that summary."));
        self.messages.extend(tail);
        self.last_prompt_tokens = 0;
        self.journal.emit(Event::Compacted { before, after: self.messages.len() }, tx);
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
