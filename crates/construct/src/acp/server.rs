//! `construct acp`: construct as an ACP agent on stdin and stdout.
//!
//! The design choice: one reader loop owns the sessions and never blocks on a
//! turn. A `session/prompt` runs as its own task so `session/cancel` (and
//! stdin closing) can interrupt it; every line out goes through one writer
//! task, so a turn's updates always precede its response.

use super::map::{self, EVENT_METHOD, META_KEY};
use super::{Start, VERSION, rpc};
use crate::agent::Agent;
use crate::config::{Config, Paths};
use crate::event::Event;
use anyhow::Result;
use rpc::Incoming;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, mpsc, watch};
use tokio::task::JoinSet;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// `construct acp`: the user's own config and sandbox policy.
    User,
    /// `construct __child --acp`: one session, set up by the parent.
    Child,
}

struct Session {
    agent: Arc<Mutex<Agent>>,
    cancel: watch::Sender<bool>,
}

type Out = mpsc::UnboundedSender<String>;

pub async fn run(cfg: Config, paths: Paths, mode: Mode) -> Result<()> {
    let (out, mut lines_out) = mpsc::unbounded_channel::<String>();
    let writer = tokio::spawn(async move {
        let mut w = tokio::io::stdout();
        while let Some(l) = lines_out.recv().await {
            if w.write_all(l.as_bytes()).await.is_err() || w.flush().await.is_err() {
                break;
            }
        }
    });
    let mut srv = Server { cfg, paths, mode, out, sessions: HashMap::new(), lossless: false };
    let mut turns = JoinSet::new();
    let mut input = BufReader::new(tokio::io::stdin()).lines();
    while let Some(line) = input.next_line().await? {
        match rpc::parse(&line) {
            Some(Incoming::Request { id, method, params }) => srv.request(id, &method, params, &mut turns).await,
            Some(Incoming::Notification { method, params }) if method == "session/cancel" => {
                if let Some(s) = params["sessionId"].as_str().and_then(|id| srv.sessions.get(id)) {
                    s.cancel.send_replace(true);
                }
            }
            _ => {}
        }
    }
    // The client is gone: stop what is running, let it wrap up, then leave.
    for s in srv.sessions.values() {
        s.cancel.send_replace(true);
    }
    while turns.join_next().await.is_some() {}
    if srv.mode == Mode::User && srv.cfg.learn.enabled {
        for s in srv.sessions.values() {
            let a = s.agent.lock().await;
            if crate::learn::kind_for(a.user_turns, a.tool_calls).is_some() {
                crate::learn::spawn_detached(&a.journal.session);
            }
        }
    }
    drop(srv);
    let _ = writer.await;
    Ok(())
}

struct Server {
    cfg: Config,
    paths: Paths,
    mode: Mode,
    out: Out,
    sessions: HashMap<String, Session>,
    /// The client asked for every `Event` whole (see `map`).
    lossless: bool,
}

impl Server {
    fn reply(&self, id: &Value, r: Result<Value, (i64, String)>) {
        let _ = self.out.send(match r {
            Ok(v) => rpc::ok(id, v),
            Err((code, m)) => rpc::err(id, code, &m),
        });
    }

    async fn request(&mut self, id: Value, method: &str, params: Value, turns: &mut JoinSet<()>) {
        match method {
            "initialize" => {
                self.lossless = params["clientCapabilities"]["_meta"][META_KEY]["events"] == json!(true);
                let result = json!({
                    "protocolVersion": VERSION,
                    "agentInfo": { "name": "construct", "title": "construct", "version": env!("CARGO_PKG_VERSION") },
                    "agentCapabilities": {
                        "loadSession": false,
                        "promptCapabilities": { "image": false, "audio": false, "embeddedContext": true },
                        "mcpCapabilities": { "http": false, "sse": false },
                        "_meta": { META_KEY: { "events": true } },
                    },
                    "authMethods": [],
                });
                self.reply(&id, Ok(result));
            }
            "session/new" => {
                let r = self.new_session(&params).map(|sid| json!({ "sessionId": sid }));
                self.reply(&id, r.map_err(|e| (rpc::INVALID_PARAMS, format!("{e:#}"))));
            }
            "session/prompt" => {
                let sid = params["sessionId"].as_str().unwrap_or_default();
                let Some(s) = self.sessions.get(sid) else {
                    return self.reply(&id, Err((rpc::INVALID_PARAMS, format!("no session {sid:?}"))));
                };
                let blocks = params["prompt"].as_array().cloned().unwrap_or_default();
                turns.spawn(prompt(
                    s.agent.clone(),
                    sid.to_string(),
                    id,
                    map::prompt_text(&blocks),
                    self.out.clone(),
                    self.lossless,
                ));
            }
            _ => self.reply(&id, Err((rpc::METHOD_NOT_FOUND, format!("unknown method {method}")))),
        }
    }

    fn new_session(&mut self, params: &Value) -> Result<String> {
        let cwd = params["cwd"].as_str().filter(|c| !c.is_empty()).map(PathBuf::from);
        let Some(cwd) = cwd else { anyhow::bail!("cwd is required") };
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
        // The journal's session id is the ACP session id, so the record and
        // the wire name a session the same way.
        let sid = agent.journal.session.clone();
        let cancel = agent.canceller();
        self.sessions.insert(sid.clone(), Session { agent: Arc::new(Mutex::new(agent)), cancel });
        Ok(sid)
    }
}

/// Run one turn, stream it, then answer the request.
async fn prompt(agent: Arc<Mutex<Agent>>, sid: String, id: Value, text: String, out: Out, lossless: bool) {
    let catalog = agent.lock().await.catalog.clone();
    let (tx, mut rx) = mpsc::unbounded_channel::<Event>();
    let fwd_out = out.clone();
    let fwd_sid = sid.clone();
    let forward = tokio::spawn(async move {
        let (mut interrupted, mut error, mut max_steps) = (false, None::<String>, false);
        while let Some(ev) = rx.recv().await {
            match &ev {
                Event::TurnEnd { interrupted: i, .. } => interrupted = *i,
                Event::Error { message } => {
                    max_steps |= message.ends_with("(max_steps)");
                    error = Some(message.clone());
                }
                _ => {}
            }
            let line = if lossless {
                rpc::notification(EVENT_METHOD, json!({ "sessionId": fwd_sid, "event": ev }))
            } else if let Some(update) = map::update(&ev, &|name| catalog.kind_of(name)) {
                rpc::notification("session/update", json!({ "sessionId": fwd_sid, "update": update }))
            } else {
                continue;
            };
            let _ = fwd_out.send(line);
        }
        (interrupted, error, max_steps)
    });
    let result = agent.lock().await.turn(&text, &tx).await;
    drop(tx);
    let (interrupted, error, max_steps) = forward.await.unwrap_or_default();
    let reply = match (result, interrupted, error) {
        (Err(e), ..) => rpc::err(&id, rpc::INTERNAL, &format!("{e:#}")),
        (_, true, _) => rpc::ok(&id, json!({ "stopReason": "cancelled" })),
        (_, _, Some(_)) if max_steps => rpc::ok(&id, json!({ "stopReason": "max_turn_requests" })),
        (_, _, Some(e)) => rpc::err(&id, rpc::INTERNAL, &e),
        _ => rpc::ok(&id, json!({ "stopReason": "end_turn" })),
    };
    let _ = out.send(reply);
}
