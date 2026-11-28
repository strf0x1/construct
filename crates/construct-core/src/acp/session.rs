//! Per-session actor state readable while the agent is busy: transcript
//! projection, turn lifecycle, queued prompts, and idempotency keys.
//!
//! The design choice: `Server` alone mutates `Session` on its event loop,
//! never taking the agent lock during attach, replay, or idempotency lookup,
//! so live output cannot interleave with replay or question reissue.

use super::keys::{Keys, Outcome, Seen, Waiter};
use super::map;
use super::replay::Transcript;
use super::rpc;
use super::turn::Prompt;
use crate::agent::Agent;
use crate::event::Event;
use crate::tools::Catalog;
use crate::tools::boundary::BoundaryHost;
use anyhow::Result;
use serde_json::{Value, json};
use std::collections::{BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{Mutex, watch};

/// Outcome of submitting a `session/prompt` to a [`Session`].
pub(super) enum Enqueue {
    Queued,
    Waits,
    Conflict(String),
    Answer(Outcome),
}

struct Turn {
    number: u64,
    phase: Phase,
}

#[derive(PartialEq, Eq)]
enum Phase {
    Working,
    Waiting,
    Idle(Option<String>),
}

impl Turn {
    fn observe(&mut self, event: &Event, reason: &mut Option<&'static str>) {
        match event {
            Event::TurnStart => {
                self.number += 1;
                *reason = None;
            }
            Event::Error { message } => {
                if message.ends_with("(max_steps)") {
                    *reason = Some("max_turn_requests");
                } else if reason.is_none() {
                    *reason = Some("error");
                }
            }
            Event::TurnEnd { interrupted, .. } => {
                let stop = if *interrupted { "cancelled" } else { reason.unwrap_or("end_turn") };
                self.phase = Phase::Idle(Some(stop.into()));
            }
            Event::Recovered { .. } => self.phase = Phase::Idle(Some("cancelled".into())),
            _ => {}
        }
    }

    fn is_idle(&self) -> bool {
        matches!(self.phase, Phase::Idle(_))
    }

    fn value(&self, session: &str) -> Value {
        let state = match self.phase {
            Phase::Working => "working",
            Phase::Waiting => "waiting",
            Phase::Idle(_) => "idle",
        };
        let mut value = json!({ "sessionId": session, "turn": self.number, "state": state });
        if let Phase::Idle(Some(reason)) = &self.phase {
            value["stopReason"] = json!(reason);
        }
        value
    }
}

pub(super) struct Session {
    /// Reject queued commands from a closed instance of this session id.
    generation: u64,
    agent: Arc<Mutex<Agent>>,
    cancel: watch::Sender<bool>,
    catalog: Arc<Catalog>,
    /// Deck's boundary endpoint, when this session has one; the reader loop
    /// pushes late decisions here without taking the agent lock.
    boundary: Option<Arc<dyn BoundaryHost>>,
    cwd: PathBuf,
    transcript: Transcript,
    announced: BTreeSet<String>,
    turn: Turn,
    attached: u64,
    prompts: VecDeque<Prompt>,
    keys: Keys,
}

impl Session {
    /// Seed actor-owned replay, turn and key state from the agent's journal.
    /// The journal's session id is the ACP session id, so the record and the
    /// wire name it the same way.
    pub fn from_agent(agent: Agent, generation: u64, attached: u64) -> Result<(String, Self)> {
        let sid = agent.journal.session.clone();
        let entries = agent.journal.entries()?;
        let mut transcript = Transcript::default();
        let mut turn = Turn { number: 0, phase: Phase::Idle(None) };
        let mut reason = None;
        let mut keys = Keys::default();
        for entry in &entries {
            transcript.observe(&entry.event, &|name| agent.catalog.kind_of(name));
            keys.observe(&entry.event);
            turn.observe(&entry.event, &mut reason);
        }
        let cancel = agent.canceller();
        let boundary = agent.boundary();
        let catalog = agent.catalog.clone();
        let cwd = agent.cwd().to_path_buf();
        let session = Self {
            generation,
            agent: Arc::new(Mutex::new(agent)),
            cancel,
            catalog,
            boundary,
            cwd,
            transcript,
            announced: BTreeSet::new(),
            turn,
            attached,
            prompts: VecDeque::new(),
            keys,
        };
        Ok((sid, session))
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn is_attached(&self, attachment: u64) -> bool {
        self.attached == attachment
    }

    pub fn cancel(&self) {
        self.cancel.send_replace(true);
    }

    pub fn turn_value(&self, sid: &str) -> Value {
        self.turn.value(sid)
    }

    pub fn push_boundary_notice(&self, sid: &str, params: &Value) {
        if let Some(b) = &self.boundary {
            b.push_notice(params, sid);
        }
    }

    /// Attach a client to this live session, optionally computing a full
    /// replay that replaces the client's view and known tool calls.
    pub fn attach(
        &mut self,
        sid: &str,
        cwd: &Path,
        attachment: u64,
        replay: bool,
    ) -> Result<(Vec<Value>, Value), (i64, String)> {
        if self.cwd.as_os_str() != cwd.as_os_str() {
            return Err((
                rpc::INVALID_PARAMS,
                format!("session {sid} belongs to {}, not {}", self.cwd.display(), cwd.display()),
            ));
        }
        if self.attached == attachment {
            return Err((rpc::INVALID_PARAMS, format!("session {sid} is already loaded on this connection")));
        }
        let updates = if replay { self.replay() } else { vec![] };
        self.attached = attachment;
        Ok((updates, self.turn.value(sid)))
    }

    /// Replay replaces the client's view, including its known tool calls.
    fn replay(&mut self) -> Vec<Value> {
        let updates = self.transcript.updates(true, &|name| self.catalog.kind_of(name));
        self.announced = updates.iter().filter_map(|u| u["toolCallId"].as_str().map(str::to_string)).collect();
        updates
    }

    /// A call created by replay already has a client row when execution starts.
    fn update(&mut self, event: &Event) -> Option<Value> {
        let mut update = map::update(event, &|name| self.catalog.kind_of(name))?;
        if let Event::ToolStart { id, .. } = event
            && !self.announced.insert(id.clone())
        {
            update["sessionUpdate"] = json!("tool_call_update");
        }
        Some(update)
    }

    /// Check idempotency across queued, running and recorded prompts; queue
    /// only new (or unkeyed) prompts.
    pub fn enqueue_prompt(&mut self, id: Value, attachment: u64, text: String, key: Option<String>) -> Enqueue {
        if let Some(k) = &key {
            let waiter = (id.clone(), attachment);
            if let Some(p) = self.prompts.iter_mut().find(|p| p.key.as_deref() == Some(k.as_str())) {
                if p.text == text {
                    p.waiters.push(waiter);
                    return Enqueue::Waits;
                }
                return Enqueue::Conflict(k.clone());
            }
            match self.keys.check(k, &text, !self.turn.is_idle(), waiter) {
                Seen::New => {}
                Seen::Waits => return Enqueue::Waits,
                Seen::Conflict => return Enqueue::Conflict(k.clone()),
                Seen::Answer(outcome) => return Enqueue::Answer(outcome),
            }
        }
        self.prompts.push_back(Prompt { generation: self.generation, id, attachment, text, key, waiters: vec![] });
        Enqueue::Queued
    }

    /// Pop and activate the next queued prompt if this session is idle.
    pub fn start_next_prompt(&mut self) -> Option<(Arc<Mutex<Agent>>, Prompt)> {
        if !self.turn.is_idle() {
            return None;
        }
        let mut prompt = self.prompts.pop_front()?;
        self.keys.start(prompt.key.clone(), prompt.text.clone(), std::mem::take(&mut prompt.waiters));
        // Cleared here, in the reader loop, not when the turn task gets the
        // agent: a `session/cancel` read before the task starts must stick.
        self.cancel.send_replace(false);
        self.turn.number += 1;
        self.turn.phase = Phase::Working;
        Some((self.agent.clone(), prompt))
    }

    /// Advance replay/key state with a live event and return the wire line for
    /// the current attachment, if attached and representable.
    pub fn observe_event(&mut self, event: &Event, attachment: u64, wire: map::Wire, sid: &str) -> Option<String> {
        // Recovered is also sent from Agent's pre-turn backlog. It was already
        // seeded from the journal at registration.
        if !matches!(event, Event::Recovered { .. }) {
            self.transcript.observe(event, &|name| self.catalog.kind_of(name));
            self.keys.observe(event);
        }
        if self.attached != attachment {
            return None;
        }
        if wire.lossless {
            Some(rpc::notification(wire.event, json!({ "sessionId": sid, "event": event.private() })))
        } else {
            self.update(event)
                .map(|update| rpc::notification("session/update", json!({ "sessionId": sid, "update": update })))
        }
    }

    /// Update waiting/working phase while a turn is active. Returns `true`
    /// when the phase changed and a turn notice should be sent.
    pub fn set_waiting(&mut self, waiting: bool) -> bool {
        if self.turn.is_idle() {
            return false;
        }
        let next = if waiting { Phase::Waiting } else { Phase::Working };
        if next != self.turn.phase {
            self.turn.phase = next;
            return true;
        }
        false
    }

    /// Complete the active turn and return any duplicate-key waiters for it.
    pub fn finish_turn(&mut self, result: &Outcome) -> Vec<Waiter> {
        self.transcript.finish_partial();
        self.turn.phase = Phase::Idle(Some(result.as_ref().copied().unwrap_or("error").to_string()));
        self.keys.finish(result)
    }

    /// Spawn detached learning on shutdown when this session earned it.
    pub async fn spawn_learn_if_needed(&self) {
        let a = self.agent.lock().await;
        if crate::learn::kind_for(a.user_turns, a.tool_calls).is_some() {
            crate::learn::spawn_detached(&a.journal.session);
        }
    }
}
