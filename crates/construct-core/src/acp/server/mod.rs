//! `construct acp`: construct as an ACP agent, on stdin and stdout or on any
//! line transport (a peer's `acp` channel, #362).
//!
//! The design choice: one reader loop owns the sessions and never blocks on a
//! turn. A `session/prompt` runs as its own task so `session/cancel` (and
//! the input closing) can interrupt it; every line out goes through one
//! channel, so a turn's updates always precede its response.
//!
//! A transport that can drop and come back (a peer's WebSocket) says so with
//! [`Input::Detached`]: the attachment ends as a new `initialize` would end
//! it, and sessions, running turns and unanswered questions stay until a
//! client attaches again (ADR 0071). Only the input closing shuts down.
//!
//! On stdio (`stdio.rs`) the protocol owns stdout (ADR 0035).
//!
//! Disk sessions outlive the process: `session/load` and `session/resume` build
//! the agent through the same `Agent::new(..., Some(id))` path `--resume`
//! uses, so ownership, recovery, the directory check and the saved model are
//! decided once. `load` replays the recorded conversation first; `resume`
//! does not, because the client still has it.
//!
//! Live loads instead read actor-owned replay state without taking the agent
//! lock. The actor consumes turn events and questions alongside requests, so
//! replay and its response cannot interleave with live output.
//!
//! A session's permission questions go to the client (`ask`), whose responses
//! this same loop reads, so a question never waits on a turn.

mod load;
mod turns;

use super::deck;
use super::map;
use super::questions::Questions;
use super::session::Session;
use super::turn::Command;
use super::{VERSION, cwd, rpc};
use crate::agent::Agent;
use crate::config::{Config, Paths};
use crate::tools::ToolSources;
use anyhow::Result;
use rpc::Incoming;
use serde_json::{Value, json};
use std::collections::HashMap;
use tokio::sync::mpsc;
use tokio::task::JoinSet;

type Out = mpsc::UnboundedSender<String>;

/// What reaches the server from its transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    /// One JSON-RPC line from the client.
    Line(String),
    /// The transport dropped. The attachment ends; nothing is cancelled.
    Detached,
}

/// The ACP agent over any line transport: requests arrive on `input`, every
/// frame leaves on `out`. Returns once `input` closes (or `out`'s reader is
/// gone) and every turn has drained.
pub async fn serve(
    cfg: Config,
    paths: Paths,
    sources: ToolSources,
    mut input: mpsc::UnboundedReceiver<Input>,
    out: Out,
) -> Result<()> {
    let (commands, mut pending) = mpsc::unbounded_channel();
    let mut srv = Server {
        cfg,
        paths,
        sources,
        out,
        commands,
        questions: Questions::default(),
        sessions: HashMap::new(),
        wire: map::Wire::legacy(),
        attachment: 0,
        next_session: 0,
        closing: false,
    };
    let mut turns = JoinSet::new();
    loop {
        tokio::select! {
            got = input.recv() => match got {
                Some(Input::Line(line)) => srv.line(&line, &mut turns),
                Some(Input::Detached) => srv.detach(),
                None => break,
            },
            Some(command) = pending.recv() => srv.command(command, &mut turns),
            Some(_) = turns.join_next(), if !turns.is_empty() => {}
            _ = srv.out.closed() => break,
        }
    }
    // Continue consuming turn output during shutdown, but start no queued turns.
    srv.closing = true;
    for s in srv.sessions.values() {
        s.cancel();
    }
    srv.questions.close();
    while !turns.is_empty() {
        tokio::select! {
            Some(command) = pending.recv() => srv.command(command, &mut turns),
            _ = turns.join_next() => {}
        }
    }
    while let Ok(command) = pending.try_recv() {
        srv.command(command, &mut turns);
    }
    if srv.cfg.learn.enabled {
        for s in srv.sessions.values() {
            s.spawn_learn_if_needed().await;
        }
    }
    Ok(())
}

struct Server {
    cfg: Config,
    paths: Paths,
    /// Tools the composition root adds to every session's catalog.
    sources: ToolSources,
    out: Out,
    questions: Questions,
    commands: mpsc::UnboundedSender<Command>,
    attachment: u64,
    next_session: u64,
    closing: bool,
    sessions: HashMap<String, Session>,
    /// What this attachment's client parses (see `map::Wire`).
    wire: map::Wire,
}

impl Server {
    fn line(&mut self, line: &str, turns: &mut JoinSet<()>) {
        match rpc::parse(line) {
            Some(Incoming::Request { id, method, params }) => self.request(id, &method, params, turns),
            Some(Incoming::Response { id, result }) => {
                if let Some(sid) = self.questions.answer(&id, result) {
                    self.waiting_changed(&sid);
                }
            }
            Some(Incoming::Notification { method, params }) if method == "session/cancel" => {
                if let Some(s) = params["sessionId"].as_str().and_then(|id| self.sessions.get(id)) {
                    s.cancel();
                }
            }
            Some(Incoming::Notification { method, params })
                if matches!(method.as_str(), map::BOUNDARY_METHOD | map::LEGACY_BOUNDARY_METHOD) =>
            {
                deck::boundary_notice(self.sessions.iter().map(|(sid, s)| (sid.as_str(), s)), &params);
            }
            _ => {}
        }
    }

    /// The client is gone but may come back: end its attachment exactly as a
    /// new `initialize` would (no output for it, its question ids retired),
    /// and keep every session, turn and question for the next one.
    fn detach(&mut self) {
        self.attachment += 1;
        self.questions.retire();
    }

    fn reply(&self, id: &Value, r: Result<Value, (i64, String)>) {
        let _ = self.out.send(match r {
            Ok(v) => rpc::ok(id, v),
            Err((code, m)) => rpc::err(id, code, &m),
        });
    }

    fn request(&mut self, id: Value, method: &str, params: Value, turns: &mut JoinSet<()>) {
        match method {
            "initialize" => {
                self.detach();
                self.wire = map::Wire::for_client(&params["clientCapabilities"]["_meta"]);
                let caps = deck::capabilities(&self.cfg);
                let result = json!({
                    "protocolVersion": VERSION,
                    "agentInfo": { "name": "construct", "title": "construct", "version": env!("CARGO_PKG_VERSION") },
                    "agentCapabilities": caps,
                    "authMethods": [],
                });
                self.reply(&id, Ok(result));
                for sid in self.sessions.keys() {
                    self.turn_notice(sid);
                }
            }
            "session/new" => {
                let r = self.new_session(&params).map(|sid| json!({ "sessionId": sid }));
                self.reply(&id, r.map_err(|e| (rpc::INVALID_PARAMS, format!("{e:#}"))));
            }
            "session/load" => self.load(&id, &params, true),
            "session/resume" => self.load(&id, &params, false),
            "session/close" => {
                // Idempotent: a session this connection never had is closed.
                let sid = params["sessionId"].as_str().unwrap_or_default();
                self.questions.close_session(sid);
                if let Some(s) = self.sessions.remove(sid) {
                    s.cancel();
                }
                self.reply(&id, Ok(json!({})));
            }
            "session/prompt" => self.prompt(id, &params, turns),
            _ => self.reply(&id, Err((rpc::METHOD_NOT_FOUND, format!("unknown method {method}")))),
        }
    }

    fn new_session(&mut self, params: &Value) -> Result<String> {
        let cwd = cwd(params)?;
        let agent = Agent::new_with_tools(self.cfg.clone(), self.paths.clone(), cwd, None, None, &self.sources)?;
        self.register(agent)
    }

    /// Take a built agent into the session table.
    fn register(&mut self, agent: Agent) -> Result<String> {
        self.next_session += 1;
        let (sid, session) = Session::from_agent(agent, self.next_session, self.attachment)?;
        self.sessions.insert(sid.clone(), session);
        Ok(sid)
    }
}
