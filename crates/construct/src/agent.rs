//! The agent loop. One user turn: route, stream the model, run the tools it
//! asks for (read-only ones in parallel), repeat until it answers in text.
//! Everything the UI sees arrives as an `Event`, so the TUI, the headless
//! runner and the tests observe the same thing, journaled on the way out.

mod compaction;
mod cutoff;
mod deck;
mod limits;
mod restore;
mod tools;
mod user_turn;
mod view;
mod wake;

use crate::child::Origin;
use crate::compact::{self, Reason};
use crate::config::{Config, Paths};
use crate::journal::{self, notes::Coalescer};
use crate::llm::{self, Delta, Message, Usage};
use crate::manifest::Manifest;
use crate::router::{self, Route};
use crate::tools::{Catalog, Ctx, ENABLE_TOOLS, Group};
use anyhow::Result;
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
    pub context: Box<dyn crate::context::Context>,
    pub groups: BTreeSet<Group>,
    pub journal: Arc<journal::Writer>,
    /// Journaled before there was a UI to send them to; sent at the first turn.
    unsent: Vec<Event>,
    pub usage: Usage,
    pub last_prompt_tokens: u64,
    /// The window a provider stated when it rejected a prompt, if smaller than configured.
    window_seen: Option<u64>,
    /// Request bytes per prompt token, as the compaction planner sizes candidates.
    bytes_per_token: f64,
    pub turns_since_memory: u32,
    pub tool_calls_since_skill: u32,
    pub user_turns: u32,
    pub tool_calls: u32,
    cancel_tx: watch::Sender<bool>,
    /// What the provider client recorded without failing, until the model call ends.
    notes: Coalescer,
}

impl Agent {
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
        cfg.select_model(id)?;
        let ep = cfg.endpoint();
        if ep != self.cfg.endpoint() {
            self.client = llm::Client::new(&ep, ep.require(ep.key())?)?;
            self.client.warm();
        }
        self.cfg = cfg;
        // A limit a provider stated about the last model says nothing of this one.
        self.window_seen = None;
        self.manifest = Manifest::new(&self.cfg, &self.catalog);
        self.journal.emit(
            Event::Model {
                model: self.cfg.model.clone(),
                provider: self.cfg.provider_name(),
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
        defs.extend(self.context.tools());
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
            .context
            .log()
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

    /// One turn. `text` is what the user typed; `None` is a turn that job
    /// completions start (`wake`), which has no user message to route or answer.
    async fn run_turn(
        &mut self,
        text: Option<&str>,
        invoked: Option<(&str, String, String)>,
        tx: &Events,
    ) -> Result<()> {
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
        if let Some(text) = text {
            self.ctx.jobs.activate_recovered();
            self.begin_user_turn(text, invoked, tx).await;
        }
        self.deliver_jobs(tx);

        let mut steps = 0u32;
        let mut turn_usage = Usage::default();
        let mut interrupted = false;
        // One forced cut and resend per step when the provider says the
        // prompt is too long; a second rejection ends the turn.
        let (mut overflowed, mut cutoffs) = (false, 0);
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
            // A late boundary decision rides in before this request, whether
            // it arrived mid-turn or between turns (ADR 0004 §6).
            self.deliver_boundary(tx);
            if !overflowed {
                self.compact(&defs, Reason::Threshold, tx).await;
            }
            steps += 1;
            let body = self.body(self.context.messages(), &defs);
            let bytes = body.len() as u64;
            let completion = match self.step(body, &defs, tx).await {
                Ok(Some(mut c)) => {
                    self.price(&self.cfg.model, &mut c.usage);
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
            if !calls.is_empty() {
                self.run_tools(&calls, tx).await;
            } else if !self.go_on_after_cutoff(&completion.finish_reason, &mut cutoffs, tx) {
                break;
            }
        }
        self.notes.flush_suppressed(&self.journal, tx);
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
        self.context.push(m);
    }

    /// The request body for `messages` with these tools, as sent.
    fn body(&self, messages: &[Message], defs: &[Value]) -> Vec<u8> {
        self.client.body(&llm::Request {
            model: &self.cfg.wire_id(&self.cfg.model),
            messages,
            tools: defs,
            max_tokens: self.cfg.max_tokens,
            temperature: self.cfg.temperature,
        })
    }

    /// Learn bytes per token from a request the provider counted. Small
    /// requests are skipped: fixed overheads would skew the ratio.
    fn measure(&mut self, bytes: u64, prompt_tokens: u64) {
        if let Some(ratio) = limits::ratio(bytes, prompt_tokens) {
            self.bytes_per_token = ratio;
        }
    }

    /// One model call. Returns None when interrupted.
    async fn step(&mut self, body: Vec<u8>, defs: &[Value], tx: &Events) -> Result<Option<llm::Completion>> {
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
        let out = {
            let fut = self.client.stream_body(body, Some(&dtx), &mut self.notes);
            tokio::pin!(fut);
            let mut cancel = self.ctx.cancel.clone();
            loop {
                tokio::select! {
                    biased;
                    d = drx.recv() => if let Some(d) = d { forward(d, tx) },
                    r = &mut fut => {
                        while let Ok(d) = drx.try_recv() { forward(d, tx) }
                        break r.map(Some);
                    }
                    _ = cancel.changed() => if *cancel.borrow() { break Ok(None) },
                }
            }
        };
        // Whether the call succeeded, failed or was cut short, what it
        // recorded on the way is one row per kind.
        self.notes.flush(&self.journal, tx);
        out
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
