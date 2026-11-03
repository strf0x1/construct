//! Session state readable while the agent is busy. The server actor alone
//! advances the transcript and writes output, making attach one indivisible
//! replay, question reissue and response relative to live delivery.

use super::ask::Answer;
use super::map::{self, EVENT_METHOD};
use super::replay::Transcript;
use super::rpc;
use super::server::{Mode, Server};
use super::turn::{self, Prompt};
use crate::agent::Agent;
use crate::event::Event;
use crate::permission::Request;
use crate::tools::Catalog;
use serde_json::{Value, json};
use std::collections::{BTreeSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{Mutex, oneshot, watch};
use tokio::task::JoinSet;

pub(super) const TURN_METHOD: &str = "_construct/turn";

pub(super) enum Command {
    Event { session: String, generation: u64, event: Box<Event> },
    Ask { key: u64, session: String, generation: u64, request: Request, answer: oneshot::Sender<Answer> },
    Forget(u64),
    Finished { session: String, prompt: Prompt, result: Result<&'static str, String> },
}

pub(super) struct Session {
    /// Reject queued commands from a closed instance of this session id.
    pub generation: u64,
    pub agent: Arc<Mutex<Agent>>,
    pub cancel: watch::Sender<bool>,
    pub catalog: Arc<Catalog>,
    /// Deck's boundary endpoint, when this session has one; the reader loop
    /// pushes late decisions here without taking the agent lock.
    pub boundary: Option<Arc<crate::tools::boundary::Boundary>>,
    pub cwd: PathBuf,
    pub transcript: Transcript,
    pub announced: BTreeSet<String>,
    pub turn: Turn,
    pub attached: u64,
    pub prompts: VecDeque<Prompt>,
}

pub(super) struct Turn {
    pub number: u64,
    pub phase: Phase,
}

#[derive(PartialEq, Eq)]
pub(super) enum Phase {
    Working,
    Waiting,
    Idle(Option<String>),
}

impl Turn {
    pub fn value(&self, session: &str) -> Value {
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

impl Session {
    /// Replay replaces the client's view, including its known tool calls.
    pub fn replay(&mut self) -> Vec<Value> {
        let updates = self.transcript.updates(true, &|name| self.catalog.kind_of(name));
        self.announced = updates.iter().filter_map(|u| u["toolCallId"].as_str().map(str::to_string)).collect();
        updates
    }

    /// A call created by replay already has a client row when execution starts.
    pub fn update(&mut self, event: &Event) -> Option<Value> {
        let mut update = map::update(event, &|name| self.catalog.kind_of(name))?;
        if let Event::ToolStart { id, .. } = event
            && !self.announced.insert(id.clone())
        {
            update["sessionUpdate"] = json!("tool_call_update");
        }
        Some(update)
    }
}

impl Server {
    pub(super) fn turn_notice(&self, sid: &str) {
        if self.mode == Mode::User
            && let Some(s) = self.sessions.get(sid)
        {
            let _ = self.out.send(rpc::notification(TURN_METHOD, s.turn.value(sid)));
        }
    }

    pub(super) fn waiting_changed(&mut self, sid: &str) {
        if let Some(s) = self.sessions.get_mut(sid)
            && !matches!(s.turn.phase, Phase::Idle(_))
        {
            let next = if self.questions.waiting(sid) { Phase::Waiting } else { Phase::Working };
            if next != s.turn.phase {
                s.turn.phase = next;
                self.turn_notice(sid);
            }
        }
    }

    pub(super) fn start_prompt(&mut self, sid: &str, turns: &mut JoinSet<()>) {
        if self.closing {
            return;
        }
        let Some(s) = self.sessions.get_mut(sid) else { return };
        if !matches!(s.turn.phase, Phase::Idle(_)) {
            return;
        }
        let Some(prompt) = s.prompts.pop_front() else { return };
        s.turn.number += 1;
        s.turn.phase = Phase::Working;
        turns.spawn(turn::run(
            s.agent.clone(),
            sid.to_string(),
            prompt,
            self.commands.clone(),
            self.mode == Mode::User,
        ));
        self.turn_notice(sid);
    }

    pub(super) fn command(&mut self, command: Command, turns: &mut JoinSet<()>) {
        match command {
            Command::Event { session, generation, event } => {
                let Some(s) = self.sessions.get_mut(&session).filter(|s| s.generation == generation) else { return };
                // Recovered is also sent from Agent's pre-turn backlog. It
                // was already seeded from the journal at registration.
                if !matches!(*event, Event::Recovered { .. }) {
                    s.transcript.observe(&event, &|name| s.catalog.kind_of(name));
                }
                if s.attached != self.attachment {
                    return;
                }
                let line = if self.lossless {
                    rpc::notification(EVENT_METHOD, json!({ "sessionId": session, "event": event }))
                } else if let Some(update) = s.update(&event) {
                    rpc::notification("session/update", json!({ "sessionId": session, "update": update }))
                } else {
                    return;
                };
                let _ = self.out.send(line);
            }
            Command::Ask { key, session, generation, request, answer } => {
                if self.closing
                    || answer.is_closed()
                    || !self.sessions.get(&session).is_some_and(|s| s.generation == generation)
                {
                    return;
                }
                self.questions.insert(key, session.clone(), request, answer);
                self.waiting_changed(&session);
                if self.sessions.get(&session).is_some_and(|s| s.attached == self.attachment) {
                    self.questions.send(&session, &self.out);
                }
            }
            Command::Forget(key) => {
                if let Some(sid) = self.questions.forget(key) {
                    self.waiting_changed(&sid);
                }
            }
            Command::Finished { session, prompt, result } => {
                if let Some(s) = self.sessions.get_mut(&session).filter(|s| s.generation == prompt.generation) {
                    s.transcript.finish_partial();
                    s.turn.phase = Phase::Idle(Some(result.as_ref().copied().unwrap_or("error").to_string()));
                    self.turn_notice(&session);
                }
                if prompt.attachment == self.attachment {
                    self.reply(
                        &prompt.id,
                        result.map(|reason| json!({ "stopReason": reason })).map_err(|e| (rpc::INTERNAL, e)),
                    );
                }
                if self.sessions.get(&session).is_some_and(|s| s.generation == prompt.generation) {
                    self.start_prompt(&session, turns);
                }
            }
        }
    }
}
