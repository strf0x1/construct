//! `construct acp`: construct as an ACP agent on stdin and stdout.
//!
//! The design choice: one reader loop owns the sessions and never blocks on a
//! turn. A `session/prompt` runs as its own task so `session/cancel` (and
//! stdin closing) can interrupt it; every line out goes through one writer
//! thread, so a turn's updates always precede its response.
//!
//! The protocol owns stdout: the writer keeps a private duplicate of fd 1 and
//! fd 1 itself is pointed at stderr, so a `println!` or a dependency's log line
//! becomes a diagnostic instead of a frame a strict client cannot parse (#162).
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

use super::cwd;
use super::live::{Command, Phase, Session, Turn};
use super::map::{self, META_KEY};
use super::questions::Questions;
use super::replay::Transcript;
use super::turn::Prompt;
use super::{Start, VERSION, rpc};
use crate::agent::Agent;
use crate::config::{Config, Paths};
use anyhow::Result;
use rpc::Incoming;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::Write;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::{Mutex, mpsc};
use tokio::task::JoinSet;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// `construct acp`: the user's own config and sandbox policy.
    User,
    /// `construct __child --acp`: one session, set up by the parent.
    Child,
}

type Out = mpsc::UnboundedSender<String>;

pub async fn run(cfg: Config, paths: Paths, mode: Mode) -> Result<()> {
    let (out, mut lines_out) = mpsc::unbounded_channel::<String>();
    // Only this loop writes protocol frames; diagnostic writes go to stderr
    // because `claim_stdout` points fd 1 there (ADR 0035).
    let protocol = super::claim_stdout();
    let writer = std::thread::spawn(move || {
        let mut w: Box<dyn Write> = match protocol {
            Some(fd) => Box::new(std::fs::File::from(fd)),
            None => Box::new(std::io::stdout()),
        };
        while let Some(l) = lines_out.blocking_recv() {
            if w.write_all(l.as_bytes()).and_then(|_| w.flush()).is_err() {
                break;
            }
        }
    });
    let (commands, mut pending) = mpsc::unbounded_channel();
    let mut srv = Server {
        cfg,
        paths,
        mode,
        out,
        commands,
        questions: Questions::default(),
        sessions: HashMap::new(),
        lossless: false,
        attachment: 0,
        next_session: 0,
        closing: false,
    };
    let mut turns = JoinSet::new();
    let mut input = BufReader::new(tokio::io::stdin()).lines();
    loop {
        tokio::select! {
            line = input.next_line() => {
                let Some(line) = line? else { break };
                match rpc::parse(&line) {
                    Some(Incoming::Request { id, method, params }) => srv.request(id, &method, params, &mut turns),
                    Some(Incoming::Response { id, result }) => {
                        if let Some(sid) = srv.questions.answer(&id, result) { srv.waiting_changed(&sid); }
                    }
                    Some(Incoming::Notification { method, params }) if method == "session/cancel" => {
                        if let Some(s) = params["sessionId"].as_str().and_then(|id| srv.sessions.get(id)) {
                            s.cancel.send_replace(true);
                        }
                    }
                    Some(Incoming::Notification { method, params }) if method == map::BOUNDARY_METHOD => {
                        srv.boundary_notice(params);
                    }
                    _ => {}
                }
            }
            Some(command) = pending.recv() => srv.command(command, &mut turns),
            Some(_) = turns.join_next(), if !turns.is_empty() => {}
            _ = srv.out.closed() => break,
        }
    }
    // Continue consuming turn output during shutdown, but start no queued turns.
    srv.closing = true;
    for s in srv.sessions.values() {
        s.cancel.send_replace(true);
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
    if srv.mode == Mode::User && srv.cfg.learn.enabled {
        for s in srv.sessions.values() {
            let a = s.agent.lock().await;
            if crate::learn::kind_for(a.user_turns, a.tool_calls).is_some() {
                crate::learn::spawn_detached(&a.journal.session);
            }
        }
    }
    drop(srv);
    let _ = writer.join();
    Ok(())
}

pub(super) struct Server {
    pub(super) cfg: Config,
    pub(super) paths: Paths,
    pub(super) mode: Mode,
    pub(super) out: Out,
    pub(super) questions: Questions,
    pub(super) commands: mpsc::UnboundedSender<Command>,
    pub(super) attachment: u64,
    next_session: u64,
    pub(super) closing: bool,
    pub(super) sessions: HashMap<String, Session>,
    /// The client asked for every `Event` whole (see `map`).
    pub(super) lossless: bool,
}

impl Server {
    pub(super) fn reply(&self, id: &Value, r: Result<Value, (i64, String)>) {
        let _ = self.out.send(match r {
            Ok(v) => rpc::ok(id, v),
            Err((code, m)) => rpc::err(id, code, &m),
        });
    }

    fn request(&mut self, id: Value, method: &str, params: Value, turns: &mut JoinSet<()>) {
        // A delegated child has one session, set up by its parent; loading
        // another would build a user-config agent inside its boundary.
        if self.mode == Mode::Child && matches!(method, "session/load" | "session/resume" | "session/close") {
            return self.reply(&id, Err((rpc::METHOD_NOT_FOUND, format!("unknown method {method}"))));
        }
        match method {
            "initialize" => {
                self.attachment += 1;
                self.questions.retire();
                self.lossless = params["clientCapabilities"]["_meta"][META_KEY]["events"] == json!(true);
                let caps = self.capabilities();
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
                    s.cancel.send_replace(true);
                }
                self.reply(&id, Ok(json!({})));
            }
            "session/prompt" => {
                let sid = params["sessionId"].as_str().unwrap_or_default();
                let Some(s) = self.sessions.get_mut(sid) else {
                    return self.reply(&id, Err((rpc::INVALID_PARAMS, format!("no session {sid:?}"))));
                };
                let blocks = params["prompt"].as_array().cloned().unwrap_or_default();
                let prompt = Prompt {
                    generation: s.generation,
                    id,
                    attachment: self.attachment,
                    text: map::prompt_text(&blocks),
                };
                s.prompts.push_back(prompt);
                self.start_prompt(sid, turns);
            }
            _ => self.reply(&id, Err((rpc::METHOD_NOT_FOUND, format!("unknown method {method}")))),
        }
    }

    fn new_session(&mut self, params: &Value) -> Result<String> {
        let cwd = cwd(params)?;
        let agent = match self.mode {
            Mode::User => Agent::new(self.cfg.clone(), self.paths.clone(), cwd, None)?,
            Mode::Child => {
                anyhow::ensure!(self.sessions.is_empty(), "a delegated child has one session");
                let start = params["_meta"][META_KEY]["start"].clone();
                let Start { config, origin } = serde_json::from_value(start)
                    .map_err(|e| anyhow::anyhow!("session/new needs _meta.{META_KEY}.start: {e}"))?;
                Agent::child(*config, self.paths.clone(), cwd, *origin)?
            }
        };
        self.register(agent)
    }

    /// Take a built agent into the session table. The journal's session id is
    /// the ACP session id, so the record and the wire name it the same way.
    pub(super) fn register(&mut self, agent: Agent) -> Result<String> {
        let sid = agent.journal.session.clone();
        let entries = agent.journal.entries()?;
        let mut transcript = Transcript::default();
        let mut turn = Turn { number: 0, phase: Phase::Idle(None) };
        let mut reason = None;
        for entry in &entries {
            transcript.observe(&entry.event, &|name| agent.catalog.kind_of(name));
            match &entry.event {
                crate::event::Event::TurnStart => {
                    turn.number += 1;
                    reason = None;
                }
                crate::event::Event::Error { message } => {
                    if message.ends_with("(max_steps)") {
                        reason = Some("max_turn_requests");
                    } else if reason.is_none() {
                        reason = Some("error");
                    }
                }
                crate::event::Event::TurnEnd { interrupted, .. } => {
                    turn.phase =
                        Phase::Idle(Some(if *interrupted { "cancelled" } else { reason.unwrap_or("end_turn") }.into()));
                }
                crate::event::Event::Recovered { .. } => turn.phase = Phase::Idle(Some("cancelled".into())),
                _ => {}
            }
        }
        let cancel = agent.canceller();
        let boundary = agent.ctx.boundary.clone();
        self.next_session += 1;
        self.sessions.insert(
            sid.clone(),
            Session {
                generation: self.next_session,
                cancel,
                transcript,
                announced: Default::default(),
                turn,
                catalog: agent.catalog.clone(),
                boundary,
                cwd: agent.ctx.cwd.clone(),
                agent: Arc::new(Mutex::new(agent)),
                attached: self.attachment,
                prompts: Default::default(),
            },
        );
        Ok(sid)
    }
}
