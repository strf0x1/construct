//! The hub's ACP client for one peer (#362): one persistent session on the
//! peer, driven over its `acp` channel and kept across disconnects and hub
//! restarts.
//!
//! The design choice: this is a pure state machine, lines in and lines out,
//! with no socket and no lock, so every step of a reconnect is unit-tested.
//! Every attachment starts with `initialize` and, once there is a session,
//! `session/load`: the peer's live attach (ADR 0033) replays the transcript,
//! reissues unanswered questions with fresh ids and reports the turn. The one
//! prompt in flight is resent with its idempotency key (ADR 0045), so a
//! reconnect never runs it twice and its completion arrives once. Nothing
//! before the `initialize` response is read: it belongs to an attachment that
//! ended. No custom resume protocol and no journal cursor (ADR 0071).

use crate::questions::Questions;
use crate::registry::{Peer, Prompt};
use crate::view::{self, SessionView};
use construct_core::acp::{map, rpc};
use rpc::Incoming;
use serde_json::{Value, json};
use std::collections::VecDeque;

/// Most transcript updates kept per peer; the oldest drop first.
const MAX_UPDATES: usize = 4096;

#[derive(Debug, Clone, PartialEq)]
enum Phase {
    Detached,
    Initializing(u64),
    Opening { id: u64, load: bool },
    Ready,
    Failed(String),
}

/// One peer's session as the hub sees it.
pub(crate) struct Remote {
    /// Mirrored into the peer's registry row (`sync`), so a restarted hub
    /// loads the same session and resends the same prompt.
    session: Option<String>,
    turn: Option<u64>,
    prompt: Option<Prompt>,
    /// The peer's working directory, from its `Hello`.
    cwd: Option<String>,
    phase: Phase,
    next_id: u64,
    /// The request id of the prompt sent on this attachment.
    sent: Option<u64>,
    /// The last turn notification (`{sessionId, turn, state, stopReason?}`).
    state: Option<Value>,
    /// How the last prompt ended (`end_turn`, `cancelled`, `error: …`).
    last: Option<String>,
    updates: VecDeque<Value>,
    /// Open permission questions, waiting for a human.
    questions: Questions,
}

impl Remote {
    /// Start from what the registry kept. `first_id` must exceed any id an
    /// earlier hub process used (the caller passes the clock in ms).
    pub(crate) fn new(row: Option<&Peer>, first_id: u64) -> Self {
        Self {
            session: row.and_then(|r| r.session.clone()),
            turn: row.and_then(|r| r.turn),
            prompt: row.and_then(|r| r.prompt.clone()),
            cwd: None,
            phase: Phase::Detached,
            next_id: first_id,
            sent: None,
            state: None,
            last: None,
            updates: VecDeque::new(),
            questions: Questions::default(),
        }
    }

    /// Copy the persistent part into `row`; whether anything changed.
    pub(crate) fn sync(&self, row: &mut Peer) -> bool {
        let before = (row.session.clone(), row.turn, row.prompt.clone());
        row.session.clone_from(&self.session);
        row.turn = self.turn;
        row.prompt.clone_from(&self.prompt);
        before != (row.session.clone(), row.turn, row.prompt.clone())
    }

    /// A link was accepted: begin a new attachment.
    pub(crate) fn attach(&mut self, cwd: Option<String>) -> Vec<String> {
        self.cwd = cwd;
        self.detach();
        let id = self.id();
        self.phase = Phase::Initializing(id);
        let caps = json!({ "_meta": { map::META_KEY: { "turn": true } } });
        vec![rpc::request(id, "initialize", json!({ "protocolVersion": 1, "clientCapabilities": caps }))]
    }

    /// The link is gone. The peer keeps working; its question ids are retired.
    pub(crate) fn detach(&mut self) {
        self.phase = Phase::Detached;
        self.sent = None;
        self.questions.clear();
    }

    /// One line from the peer; returns the lines to send back.
    pub(crate) fn receive(&mut self, line: &str) -> Vec<String> {
        let mut out = vec![];
        let Some(msg) = rpc::parse(line) else { return out };
        if matches!(self.phase, Phase::Detached) {
            return out;
        }
        if let Phase::Initializing(want) = self.phase {
            // Everything before this response was for an earlier attachment.
            if let Incoming::Response { id, result } = msg
                && id.as_u64() == Some(want)
            {
                match result {
                    Ok(_) => self.initialized(&mut out),
                    Err(e) => self.phase = Phase::Failed(format!("initialize: {}", e.message)),
                }
            }
            return out;
        }
        match msg {
            Incoming::Response { id, result } => self.response(id.as_u64(), result, &mut out),
            Incoming::Request { id, method, params } => {
                if method == "session/request_permission" && self.ours(&params) {
                    self.questions.push(id, params);
                } else {
                    out.push(rpc::err(&id, rpc::METHOD_NOT_FOUND, &format!("the hub does not answer {method}")));
                }
            }
            Incoming::Notification { method, params } if self.ours(&params) => match method.as_str() {
                "session/update" => view::keep(&mut self.updates, params["update"].clone(), MAX_UPDATES),
                map::TURN_METHOD | map::LEGACY_TURN_METHOD => self.turned(params),
                _ => {}
            },
            Incoming::Notification { .. } => {}
        }
        out
    }

    /// Queue a prompt (one at a time); sent now if the session is ready.
    pub(crate) fn prompt(&mut self, text: String, key: String) -> Result<Vec<String>, String> {
        if let Some(p) = &self.prompt {
            return Err(format!("a prompt is still running ({:?}); cancel it or wait", p.text));
        }
        self.prompt = Some(Prompt { key, text });
        self.last = None;
        let mut out = vec![];
        self.advance(&mut out);
        Ok(out)
    }

    /// Cancel the running turn on the peer.
    pub(crate) fn cancel(&mut self) -> Result<Vec<String>, String> {
        match (&self.phase, &self.session) {
            (Phase::Ready, Some(sid)) => Ok(vec![rpc::notification("session/cancel", json!({ "sessionId": sid }))]),
            (Phase::Ready, None) => Err("no session yet".into()),
            _ => Err("the peer is not attached; its turn keeps running".into()),
        }
    }

    /// Answer open question `n` (1-based; may be omitted when only one is
    /// open) with `option`; see `Questions::answer`.
    pub(crate) fn answer(
        &mut self,
        option: &str,
        n: Option<usize>,
        expect: Option<&str>,
    ) -> Result<Vec<String>, String> {
        self.questions.answer(option, n, expect).map(|line| vec![line])
    }

    /// A prompt is in flight, or the peer's last turn report says it runs.
    pub(crate) fn working(&self) -> bool {
        self.prompt.is_some() || self.state.as_ref().is_some_and(|s| s["state"] != "idle")
    }

    /// The open permission questions, oldest first.
    pub(crate) fn questions(&self) -> Vec<view::Question> {
        self.questions.list()
    }

    pub(crate) fn view(&self, peer: &str) -> SessionView {
        SessionView {
            peer: peer.into(),
            attached: !matches!(self.phase, Phase::Detached),
            session: self.session.clone(),
            cwd: self.cwd.clone(),
            turn: self.state.clone(),
            prompt: self.prompt.as_ref().map(|p| p.text.clone()),
            last: self.last.clone(),
            error: match &self.phase {
                Phase::Failed(e) => Some(e.clone()),
                _ => None,
            },
            transcript: view::transcript(self.updates.iter()),
            questions: self.questions(),
        }
    }

    fn id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    fn ours(&self, params: &Value) -> bool {
        self.session.is_some() && params["sessionId"].as_str() == self.session.as_deref()
    }

    fn initialized(&mut self, out: &mut Vec<String>) {
        match (self.session.clone(), self.cwd.clone()) {
            (Some(sid), Some(cwd)) => {
                let id = self.id();
                // Load replays the whole transcript: the view starts over.
                self.updates.clear();
                out.push(rpc::request(id, "session/load", json!({ "sessionId": sid, "cwd": cwd, "mcpServers": [] })));
                self.phase = Phase::Opening { id, load: true };
            }
            (Some(_), None) => self.phase = Phase::Failed("the peer reported no working directory".into()),
            (None, _) => {
                self.phase = Phase::Ready;
                self.advance(out);
            }
        }
    }

    fn response(&mut self, id: Option<u64>, result: Result<Value, rpc::RpcError>, out: &mut Vec<String>) {
        if let Phase::Opening { id: want, load } = self.phase
            && id == Some(want)
        {
            match result {
                Ok(v) if load => {
                    let turn = &v["_meta"][map::META_KEY]["turn"];
                    if !turn.is_null() {
                        self.turned(turn.clone());
                    }
                }
                Ok(v) => {
                    self.session = v["sessionId"].as_str().map(str::to_string);
                    (self.turn, self.state) = (None, None);
                    self.updates.clear();
                }
                // The peer no longer has it (a wiped home): start over.
                Err(e) if load && e.code == rpc::NOT_FOUND => (self.session, self.turn) = (None, None),
                Err(e) => {
                    let what = if load { "session/load" } else { "session/new" };
                    self.phase = Phase::Failed(format!("{what}: {}", e.message));
                    return;
                }
            }
            self.phase = Phase::Ready;
            self.advance(out);
            return;
        }
        if id.is_some() && id == self.sent {
            self.last = Some(match result {
                Ok(v) => v["stopReason"].as_str().unwrap_or("end_turn").to_string(),
                Err(e) => format!("error: {}", e.message),
            });
            (self.prompt, self.sent) = (None, None);
        }
    }

    fn turned(&mut self, params: Value) {
        self.turn = params["turn"].as_u64().or(self.turn);
        if params["state"] == "idle" {
            self.questions.clear();
        }
        self.state = Some(params);
    }

    /// Send what the session needs next: a session for a waiting prompt, or
    /// the prompt itself (again, with its key, after a reconnect).
    fn advance(&mut self, out: &mut Vec<String>) {
        if self.phase != Phase::Ready || self.sent.is_some() {
            return;
        }
        let Some(p) = self.prompt.clone() else { return };
        let id = self.id();
        match (self.session.clone(), self.cwd.clone()) {
            (Some(sid), _) => {
                let meta = json!({ map::META_KEY: { map::IDEMPOTENCY_KEY: p.key } });
                let blocks = json!([{ "type": "text", "text": p.text }]);
                out.push(rpc::request(
                    id,
                    "session/prompt",
                    json!({ "sessionId": sid, "prompt": blocks, "_meta": meta }),
                ));
                self.sent = Some(id);
                view::show_prompt(&mut self.updates, &p.text, MAX_UPDATES);
            }
            (None, Some(cwd)) => {
                out.push(rpc::request(id, "session/new", json!({ "cwd": cwd, "mcpServers": [] })));
                self.phase = Phase::Opening { id, load: false };
            }
            (None, None) => self.phase = Phase::Failed("the peer reported no working directory".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(line: &str) -> Value {
        serde_json::from_str(line).unwrap()
    }

    fn reply(to: &str, result: Value) -> String {
        rpc::ok(&parse(to)["id"], result)
    }

    #[test]
    fn a_first_prompt_opens_a_session_and_its_answer_clears_it() {
        let mut r = Remote::new(None, 100);
        let init = r.attach(Some("/w".into())).remove(0);
        assert_eq!(parse(&init)["method"], "initialize");
        // A line from an earlier attachment is never read.
        assert!(r.receive(&rpc::ok(&json!(7), json!({ "stopReason": "end_turn" }))).is_empty());
        assert!(r.receive(&reply(&init, json!({}))).is_empty(), "no session until there is work");
        let new = r.prompt("hi".into(), "k1".into()).unwrap().remove(0);
        assert_eq!(
            (parse(&new)["method"].as_str(), parse(&new)["params"]["cwd"].as_str()),
            (Some("session/new"), Some("/w"))
        );
        let prompt = r.receive(&reply(&new, json!({ "sessionId": "s1" }))).remove(0);
        let p = parse(&prompt);
        assert_eq!(p["params"]["_meta"][map::META_KEY][map::IDEMPOTENCY_KEY], "k1");
        assert!(r.prompt("again".into(), "k2".into()).is_err(), "one prompt at a time");
        r.receive(&rpc::notification(map::TURN_METHOD, json!({ "sessionId": "s1", "turn": 1, "state": "working" })));
        let chunk = json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": "hello" } });
        r.receive(&rpc::notification("session/update", json!({ "sessionId": "s1", "update": chunk })));
        r.receive(&reply(&prompt, json!({ "stopReason": "end_turn" })));
        let v = r.view("old-pc");
        assert_eq!((v.prompt, v.last.as_deref(), v.transcript.as_str()), (None, Some("end_turn"), "> hi\nhello"));
        let mut row = Peer::new("old-pc", crate::registry::State::Pinned);
        assert!(r.sync(&mut row));
        assert_eq!((row.session.as_deref(), row.turn, row.prompt), (Some("s1"), Some(1), None));
    }

    #[test]
    fn a_restarted_hub_loads_the_session_and_resends_the_prompt_with_its_key() {
        let mut row = Peer::new("old-pc", crate::registry::State::Pinned);
        row.session = Some("s1".into());
        row.prompt = Some(Prompt { key: "k1".into(), text: "go".into() });
        let mut r = Remote::new(Some(&row), 5);
        let init = r.attach(Some("/w".into())).remove(0);
        let load = r.receive(&reply(&init, json!({}))).remove(0);
        assert_eq!(parse(&load)["params"], json!({ "sessionId": "s1", "cwd": "/w", "mcpServers": [] }));
        // The replay and the reissued question arrive before the load answer.
        let ask = rpc::request(
            9,
            "session/request_permission",
            json!({ "sessionId": "s1", "options": [{ "optionId": "once" }, { "optionId": "deny" }] }),
        );
        r.receive(&ask);
        let turn = json!({ "sessionId": "s1", "turn": 1, "state": "waiting" });
        let resend = r.receive(&reply(&load, json!({ "_meta": { map::META_KEY: { "turn": turn } } }))).remove(0);
        let p = parse(&resend);
        assert_eq!(
            (p["method"].as_str(), p["params"]["_meta"][map::META_KEY][map::IDEMPOTENCY_KEY].as_str()),
            (Some("session/prompt"), Some("k1"))
        );
        assert_eq!(r.view("old-pc").turn.unwrap()["state"], "waiting");
        assert_eq!(r.view("old-pc").transcript, "> go", "the resent prompt is shown");
        assert!(r.answer("always", None, None).is_err(), "only offered options");
        assert!(r.answer("once", Some(1), Some("another question")).is_err(), "the title must still match");
        let answer = parse(&r.answer("once", Some(1), Some("permission")).unwrap().remove(0));
        assert_eq!((answer["id"].as_u64(), answer["result"]["outcome"]["optionId"].as_str()), (Some(9), Some("once")));
        // A drop retires question ids; the next attach starts with initialize.
        r.receive(&ask);
        r.detach();
        assert!(r.view("old-pc").questions.is_empty());
        assert!(r.cancel().is_err());
        assert_eq!(parse(&r.attach(Some("/w".into()))[0])["method"], "initialize");
    }

    #[test]
    fn a_session_the_peer_lost_is_opened_again() {
        let mut row = Peer::new("old-pc", crate::registry::State::Pinned);
        row.session = Some("gone".into());
        let mut r = Remote::new(Some(&row), 0);
        let init = r.attach(Some("/w".into())).remove(0);
        let load = r.receive(&reply(&init, json!({}))).remove(0);
        let missing = rpc::err_data(&parse(&load)["id"], rpc::NOT_FOUND, "no session gone", json!({}));
        assert!(r.receive(&missing).is_empty());
        let new = r.prompt("hi".into(), "k".into()).unwrap().remove(0);
        assert_eq!(parse(&new)["method"], "session/new");
    }

    #[test]
    fn a_prompt_the_replay_already_shows_is_not_shown_twice() {
        let mut row = Peer::new("old-pc", crate::registry::State::Pinned);
        row.session = Some("s1".into());
        row.prompt = Some(Prompt { key: "k1".into(), text: "go".into() });
        let mut r = Remote::new(Some(&row), 5);
        let init = r.attach(Some("/w".into())).remove(0);
        let load = r.receive(&reply(&init, json!({}))).remove(0);
        for (kind, text) in [("user_message_chunk", "g"), ("user_message_chunk", "o"), ("agent_message_chunk", "on it")]
        {
            let u = json!({ "sessionUpdate": kind, "content": { "type": "text", "text": text } });
            r.receive(&rpc::notification("session/update", json!({ "sessionId": "s1", "update": u })));
        }
        assert_eq!(parse(&r.receive(&reply(&load, json!({}))).remove(0))["method"], "session/prompt");
        assert_eq!(r.view("old-pc").transcript, "> go\non it");
    }
}
