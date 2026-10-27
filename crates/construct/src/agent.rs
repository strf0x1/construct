//! The agent loop. One user turn: route, stream the model, run the tools it
//! asks for (read-only ones in parallel), repeat until it answers in text.
//! Everything the UI shows arrives as an `Event`, so the TUI, the headless
//! runner and the tests all observe the same thing, and every durable event
//! is journaled on the way out (`journal::Writer::emit`).

use crate::child::Origin;
use crate::compact::{self, Reason};
use crate::config::{Config, Paths};
use crate::journal;
use crate::llm::{self, Delta, Message, Usage};
use crate::manifest::Manifest;
use crate::router::{self, Route};
use crate::tools::{Call, Catalog, Ctx, ENABLE_TOOLS, Group, OutputKind};
use anyhow::{Result, bail};
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
    /// The window a provider stated when it rejected a prompt, if smaller
    /// than the configured one: planning uses it from then on.
    window_seen: Option<u64>,
    /// Request bytes per prompt token, measured on the last request big
    /// enough to say; what the compaction planner sizes candidates with.
    bytes_per_token: f64,
    pub turns_since_memory: u32,
    pub tool_calls_since_skill: u32,
    pub user_turns: u32,
    pub tool_calls: u32,
    cancel_tx: watch::Sender<bool>,
}

impl Agent {
    pub fn new(cfg: Config, paths: Paths, cwd: std::path::PathBuf, resume: Option<String>) -> Result<Self> {
        Self::build(cfg, paths, cwd, resume, None)
    }

    /// The session of a delegated child: its own prompt, and a head row that
    /// names who delegated.
    pub fn child(cfg: Config, paths: Paths, cwd: std::path::PathBuf, origin: Origin) -> Result<Self> {
        Self::build(cfg, paths, cwd, None, Some(origin))
    }

    fn build(
        cfg: Config,
        paths: Paths,
        cwd: std::path::PathBuf,
        resume: Option<String>,
        origin: Option<Origin>,
    ) -> Result<Self> {
        let key = match origin {
            Some(_) => crate::secrets::env_key(&cfg.api_key_secret),
            None => cfg.endpoint_key(),
        };
        if key.is_none() && !cfg.api_key_secret.is_empty() {
            bail!(crate::secrets::missing_key(&cfg.api_key_secret));
        }
        let client = llm::Client::new(&cfg, key)?;
        let memory = crate::memory::Store::new(&paths, &cfg.memory).with_project(&paths, &cwd);
        let (cancel_tx, cancel_rx) = watch::channel(false);
        let mcp = if cfg.mcp.is_empty() {
            None
        } else {
            Some(Arc::new(crate::mcp::Manager::new(cfg.mcp.clone(), &cfg.sandbox, &cwd, &paths)))
        };
        let catalog = Arc::new(Catalog::build(&cfg.tools, mcp.clone())?);
        if !cfg.agents.is_empty() {
            catalog.add(Arc::new(crate::tools::delegate::Delegate::new(&cfg, &catalog.table(), &cwd)?));
        }
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
                    parent_session: origin.as_ref().map(|o| o.parent_session.clone()),
                    parent_call_id: origin.as_ref().map(|o| o.parent_call_id.clone()),
                })?;
                unsent.push(head);
                (w, vec![])
            }
        };
        let (system, sandbox) = match origin {
            // The child's boundary was decided by its parent; `parent` is the
            // pid the helper watches, and that is this process now.
            Some(o) => (o.system, crate::sandbox::Policy { parent: std::process::id(), ..o.policy }),
            None => (crate::prompt::build(&cwd, &memory), crate::sandbox::Policy::new(&cfg.sandbox, &cwd, &paths)),
        };
        match messages.first() {
            Some(m) if m.role == "system" => messages[0] = Message::system(system),
            _ => messages.insert(0, Message::system(system)),
        }
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
            window_seen: None,
            bytes_per_token: 4.0,
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

    /// Switch to `id` for the next request: its catalog entry's window, cap
    /// and endpoint replace the last model's, or the defaults if it has no
    /// entry. A new endpoint or key means a new client; a key that doesn't
    /// resolve leaves the old model in place.
    pub fn set_model(&mut self, id: &str, tx: &Events) -> Result<()> {
        let mut cfg = self.cfg.clone();
        cfg.select_model(id);
        let moved = (&cfg.base_url, &cfg.api_key_secret, &cfg.provider, &cfg.headers)
            != (&self.cfg.base_url, &self.cfg.api_key_secret, &self.cfg.provider, &self.cfg.headers);
        if moved {
            let key = cfg.endpoint_key();
            if key.is_none() && !cfg.api_key_secret.is_empty() {
                bail!(crate::secrets::missing_key(&cfg.api_key_secret));
            }
            self.client = llm::Client::new(&cfg, key)?;
            self.client.warm();
        }
        self.cfg = cfg;
        // A limit a provider stated about the last model says nothing of this one.
        self.window_seen = None;
        self.manifest = Manifest::new(&self.cfg, &self.catalog);
        self.journal.emit(
            Event::Model {
                model: self.cfg.model.clone(),
                context_window: self.cfg.context_window,
                max_tokens: self.cfg.max_tokens,
            },
            tx,
        );
        Ok(())
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
        let skills = self.ctx.all_skills();
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
        self.run_turn(text, None, tx).await
    }

    /// A turn the user started with `/<skill> text`: the skill's SKILL.md (as
    /// the `skill` tool returns it) goes into the user message ahead of
    /// `text`, and an `invoke` event records which skill it was. A skill that
    /// is gone by now is an error and no turn.
    pub async fn invoke(&mut self, skill: &str, text: &str, tx: &Events) -> Result<()> {
        match crate::tools::skill::load(&self.ctx, skill, None) {
            Ok(body) => self.run_turn(text, Some((skill, body)), tx).await,
            Err(e) => {
                self.journal.emit(Event::Error { message: e }, tx);
                Ok(())
            }
        }
    }

    async fn run_turn(&mut self, text: &str, invoked: Option<(&str, String)>, tx: &Events) -> Result<()> {
        let started = Instant::now();
        self.cancel_tx.send_replace(false);
        self.ctx.turn_began();
        self.ctx.perms.set_sink(self.journal.clone(), tx.clone());
        for ev in std::mem::take(&mut self.unsent) {
            let _ = tx.send(ev);
        }
        self.journal.emit(Event::TurnStart, tx);
        if self.user_turns == 0 {
            self.journal.emit(Event::Manifest(self.manifest.clone()), tx);
        }
        let mut route = self.route(text).await;
        if let Some((name, _)) = &invoked {
            route.skills.retain(|(n, _)| n != name);
            // Its linked files are read with the skill tool.
            if !route.groups.contains(&Group::Skills) {
                route.groups.push(Group::Skills);
            }
        }
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
            if let Some(lib) = self.ctx.skill_lib(name) {
                lib.suggested(name);
            }
        }
        let mut content = text.to_string();
        if let Some((name, body)) = invoked {
            let scope = if self.ctx.project_skills.as_ref().is_some_and(|p| p.get(name).is_some()) {
                "project"
            } else {
                "global"
            };
            self.journal.emit(Event::Invoke { skill: name.to_string(), scope: scope.to_string() }, tx);
            content = crate::prompt::invoked_skill(name, &body, text);
        }
        if let Some(r) = crate::prompt::skill_reminder(&route.skills, &self.ctx.all_skills()) {
            content.push_str("\n\n");
            content.push_str(&r);
        }
        self.push(Message::user(content), tx);
        self.user_turns += 1;
        self.turns_since_memory += 1;

        let mut steps = 0u32;
        let mut turn_usage = Usage::default();
        let mut interrupted = false;
        // One forced cut and resend per step when the provider says the
        // prompt is too long; a second rejection ends the turn.
        let mut overflowed = false;
        loop {
            if self.cancelled() {
                interrupted = true;
                break;
            }
            if steps >= self.cfg.max_steps {
                self.journal.emit(Event::Error { message: format!("stopped after {steps} steps (max_steps)") }, tx);
                break;
            }
            let defs = self.tool_defs();
            if !overflowed {
                self.compact(&defs, Reason::Threshold, tx).await;
            }
            steps += 1;
            let body = self.body(&self.messages, &defs);
            let bytes = body.len() as u64;
            let completion = match self.step(body, &defs, tx).await {
                Ok(Some(mut c)) => {
                    // The catalog's price stands in when the provider sends none.
                    if c.usage.cost.is_none()
                        && let Some(p) = self.cfg.entry().and_then(|e| e.pricing)
                    {
                        c.usage.cost = Some(p.cost(c.usage.prompt_tokens, c.usage.completion_tokens));
                    }
                    c
                }
                Ok(None) => {
                    interrupted = true;
                    break;
                }
                Err(e) if !overflowed && compact::is_overflow(&format!("{e:#}")) => {
                    // Recovered, not an error: the `compacted` event records it.
                    let e = format!("{e:#}");
                    if let Some(t) = compact::overflow_tokens(&e) {
                        self.measure(bytes, t);
                    }
                    if let Some(w) = compact::overflow_limit(&e).filter(|&w| w < self.cfg_context_window()) {
                        self.window_seen = Some(w);
                    }
                    overflowed = true;
                    steps -= 1;
                    if !self.compact(&defs, Reason::Overflow, tx).await {
                        break;
                    }
                    continue;
                }
                Err(e) => {
                    self.journal.emit(Event::Error { message: format!("{e:#}") }, tx);
                    break;
                }
            };
            overflowed = false;
            turn_usage.add(&completion.usage);
            self.usage.add(&completion.usage);
            self.last_prompt_tokens = completion.usage.prompt_tokens;
            self.measure(bytes, completion.usage.prompt_tokens);
            self.journal.emit(
                Event::Usage {
                    prompt: completion.usage.prompt_tokens,
                    completion: completion.usage.completion_tokens,
                    cached: completion.usage.cached_tokens,
                    cache_write: completion.usage.cache_write_tokens,
                    cost: completion.usage.cost,
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
                cached: turn_usage.cached_tokens,
                cost: turn_usage.cost,
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

    /// The request body for `messages` with these tools, as sent.
    fn body(&self, messages: &[Message], defs: &[Value]) -> Vec<u8> {
        self.client.body(&llm::Request {
            model: &self.cfg.model,
            messages,
            tools: defs,
            max_tokens: self.cfg.max_tokens,
            temperature: self.cfg.temperature,
        })
    }

    /// Learn bytes per token from a request the provider counted. Small
    /// requests are skipped: fixed overheads would skew the ratio.
    fn measure(&mut self, bytes: u64, prompt_tokens: u64) {
        if prompt_tokens >= 1024 {
            self.bytes_per_token = (bytes as f64 / prompt_tokens as f64).clamp(1.0, 8.0);
        }
    }

    /// One model call. Returns None when interrupted.
    async fn step(&self, body: Vec<u8>, defs: &[Value], tx: &Events) -> Result<Option<llm::Completion>> {
        let (dtx, mut drx) = mpsc::unbounded_channel::<Delta>();
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
                let call = Call::new(&c.id, journal.clone(), tx.clone());
                let r = catalog.call_with(&ctx, &call, &c.function.name, &c.function.arguments).await;
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

    /// Cut history if the planner says so (see `compact`). Returns whether
    /// anything was cut.
    async fn compact(&mut self, defs: &[Value], reason: Reason, tx: &Events) -> bool {
        let started = Instant::now();
        let budget = compact::budget(self.cfg_context_window(), self.cfg.max_tokens);
        let ratio = self.bytes_per_token;
        let (client, model, max_tokens, temperature) =
            (self.client.clone(), self.cfg.model.clone(), self.cfg.max_tokens, self.cfg.temperature);
        let size = move |messages: &[Message]| {
            let req = llm::Request { model: &model, messages, tools: defs, max_tokens, temperature };
            (client.body(&req).len() as f64 / ratio) as u64
        };
        let Some(plan) = compact::plan(&self.messages, &size, budget, &self.cfg.compaction, reason) else {
            if reason == Reason::Overflow {
                self.journal
                    .emit(Event::Error { message: "the prompt is too long and nothing is left to cut".into() }, tx);
            }
            return false;
        };
        let (mut summary, mut fallback, mut usage) = (None, None, Usage::default());
        if let Some(upto) = plan.upto {
            let (r, u) = self.summarize(upto, budget).await;
            usage = u;
            match r {
                Ok(text) => summary = Some(text),
                Err(e) => fallback = Some(e),
            }
        }
        let cut = plan.cut(summary);
        if cut.summary.is_none() && cut.elide.is_empty() {
            self.journal
                .emit(Event::Error { message: format!("compaction failed: {}", fallback.unwrap_or_default()) }, tx);
            return false;
        }
        let before = self.messages.len();
        self.messages = compact::apply(std::mem::take(&mut self.messages), &cut);
        let tokens_after = size(&self.messages);
        self.last_prompt_tokens = 0;
        self.journal.emit(
            Event::Compacted {
                before,
                after: self.messages.len(),
                mode: if cut.summary.is_some() { "summarize" } else { "elide" }.into(),
                reason: plan.reason.label().into(),
                tokens_before: plan.tokens_before,
                tokens_after,
                budget,
                reached: tokens_after <= (self.cfg.compaction.target as f64 * budget as f64) as u64,
                ms: started.elapsed().as_millis() as u64,
                summary_prompt: usage.prompt_tokens,
                summary_completion: usage.completion_tokens,
                fallback,
                cut: Some(cut),
            },
            tx,
        );
        true
    }

    /// Summarize messages `1..upto` (past any earlier summary) with the
    /// summarizer model. Its input is capped to fit the same budget; its
    /// output limit leaves room for reasoning, and a summary cut off by that
    /// limit is refused rather than kept half-written.
    /// The usage is returned either way: a refused summary was still paid for.
    async fn summarize(&self, upto: usize, budget: u64) -> (Result<String, String>, Usage) {
        let max_chars = (budget as f64 * 0.6 * self.bytes_per_token) as usize;
        let words = compact::summary_allowance(budget) * 3 / 4;
        let req = vec![
            Message::system(format!("{}\nKeep it under {words} words.", compact::SUMMARY_SYSTEM)),
            Message::user(compact::summary_request(&self.messages[1..upto], max_chars)),
        ];
        let model = if self.cfg.compaction.model.is_empty() { &self.cfg.model } else { &self.cfg.compaction.model };
        let mut usage = Usage::default();
        // An empty answer is the model's flake (reasoning, then no text): one
        // more try is cheaper than losing the summary to a coarser cut.
        for _ in 0..2 {
            let r = self
                .client
                .complete(llm::Request {
                    model,
                    messages: &req,
                    tools: &[],
                    max_tokens: Some(compact::summary_allowance(budget) as u32 + compact::REASONING_HEADROOM),
                    temperature: None,
                })
                .await;
            let c = match r {
                Ok(c) => c,
                Err(e) => return (Err(format!("{e:#}")), usage),
            };
            usage.add(&c.usage);
            let text = c.message.content.unwrap_or_default();
            if c.finish_reason == "length" {
                return (Err(format!("the summary hit the output limit after {} chars", text.len())), usage);
            }
            if !text.trim().is_empty() {
                return (Ok(text), usage);
            }
        }
        (Err("the summarizer returned nothing, twice".into()), usage)
    }

    fn cfg_context_window(&self) -> u64 {
        let w = self.cfg.context_window.max(8192) as u64;
        self.window_seen.map_or(w, |s| s.min(w))
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
