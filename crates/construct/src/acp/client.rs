//! The parent's side of an ACP delegation: a [`Wire`] that walks a child
//! through `initialize`, `session/new` and one `session/prompt`, passes what
//! it streams up as `Event`s, and answers what it asks.
//!
//! The design choice: the child's session id is the ACP session id, and the
//! parent journals what the child's stream says. A construct child streams its
//! events whole (`_construct/event`, see `map`), so the record matches the
//! JSONL transport row for row. Any other agent only has ACP's vocabulary, so
//! the parent writes the closest honest record from it: a synthetic head, the
//! task, the text, the tool calls and their results, and the end of the turn.
//! Anything the child asks of the client that construct doesn't advertise
//! (files, terminals) is refused as an unknown method.

use super::map::{EVENT_METHOD, META_KEY};
use super::rpc::{self, Incoming};
use super::{Start, VERSION};
use crate::child::run::Outcome;
use crate::child::wire::Wire;
use crate::config::Config;
use crate::event::Event;
use crate::llm::Message;
use crate::manifest::Manifest;
use crate::tools::{Call, OutputKind};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

const INIT: u64 = 1;
const NEW: u64 = 2;
const PROMPT: u64 = 3;

enum Kind {
    /// `construct __child --acp`: set up by `_meta.construct.start`.
    Construct(Box<Start>),
    /// Another vendor's agent: journaled from ACP updates alone.
    Foreign { harness: String, manifest: Box<Manifest> },
}

pub struct Client {
    kind: Kind,
    cwd: PathBuf,
    task: String,
    allow: bool,
    /// The child promised to stream its events whole, and we asked for it.
    lossless: bool,
    session: Option<String>,
    version: String,
    // What the parent writes for a foreign agent.
    started: Instant,
    said: String,
    steps: u32,
    calls: HashMap<String, Tool>,
    /// Token use the agent reported with its stop reason, if it did.
    usage: (u64, u64, u64),
}

/// A foreign agent's tool call as it is assembled: ACP opens a call as
/// `pending` with whatever input has streamed so far, and the real input
/// arrives in later updates, so the journal's `tool_start` waits for it.
struct Tool {
    name: String,
    args: String,
    started: Option<Instant>,
}

impl Client {
    pub fn construct(cwd: &Path, task: &str, start: Start, allow: bool) -> Self {
        Self::new(Kind::Construct(Box::new(start)), cwd, task, allow)
    }

    /// `name` is the definition's, which is what the record calls the harness.
    pub fn foreign(name: &str, parent: &Config, cwd: &Path, task: &str, allow: bool) -> Self {
        let kind = Kind::Foreign { harness: name.to_string(), manifest: Box::new(Manifest::foreign(parent)) };
        Self::new(kind, cwd, task, allow)
    }

    fn new(kind: Kind, cwd: &Path, task: &str, allow: bool) -> Self {
        Self {
            kind,
            cwd: cwd.to_path_buf(),
            task: task.to_string(),
            allow,
            lossless: false,
            session: None,
            version: String::new(),
            started: Instant::now(),
            said: String::new(),
            steps: 0,
            calls: HashMap::new(),
            usage: (0, 0, 0),
        }
    }

    fn emit(&self, call: &Call, out: &mut Outcome, ev: Event) {
        out.note(&ev);
        call.forward(ev);
    }

    fn response(
        &mut self,
        id: u64,
        result: Result<Value, rpc::RpcError>,
        call: &Call,
        out: &mut Outcome,
    ) -> Vec<String> {
        let value = match result {
            Ok(v) => v,
            Err(e) => {
                out.error.get_or_insert(format!("the agent refused {}: {}", step_name(id), e.message));
                out.finished = true;
                return vec![];
            }
        };
        match id {
            INIT => {
                self.lossless = matches!(self.kind, Kind::Construct(_))
                    && value["agentCapabilities"]["_meta"][META_KEY]["events"] == json!(true);
                self.version = value["agentInfo"]["version"].as_str().unwrap_or_default().to_string();
                let mut params = json!({ "cwd": self.cwd.display().to_string(), "mcpServers": [] });
                if let Kind::Construct(start) = &self.kind {
                    params["_meta"] = json!({ META_KEY: { "start": start } });
                }
                vec![rpc::request(NEW, "session/new", params)]
            }
            NEW => {
                let Some(sid) = value["sessionId"].as_str() else {
                    out.error = Some("the agent's session/new named no session".into());
                    out.finished = true;
                    return vec![];
                };
                self.session = Some(sid.to_string());
                if let Kind::Foreign { harness, manifest } = &self.kind {
                    let head = Event::Head {
                        id: sid.to_string(),
                        harness: harness.clone(),
                        version: self.version.clone(),
                        cwd: self.cwd.display().to_string(),
                        model: String::new(),
                        manifest: (**manifest).clone(),
                        parent_session: call.session().map(str::to_string),
                        parent_call_id: Some(call.id.clone()),
                    };
                    self.emit(call, out, head);
                    self.emit(call, out, Event::User { message: Message::user(self.task.clone()) });
                    self.emit(call, out, Event::TurnStart);
                }
                let prompt = json!([{ "type": "text", "text": self.task }]);
                vec![rpc::request(PROMPT, "session/prompt", json!({ "sessionId": sid, "prompt": prompt }))]
            }
            _ => {
                let stop = value["stopReason"].as_str().unwrap_or_default();
                let n = |k: &str| value["usage"][k].as_u64().unwrap_or(0);
                self.usage = (n("inputTokens"), n("outputTokens"), n("cachedReadTokens"));
                if matches!(self.kind, Kind::Foreign { .. }) {
                    self.finish_foreign(stop, call, out);
                }
                out.finished = true;
                vec![]
            }
        }
    }

    /// The end of a foreign agent's turn, written as construct's own would be.
    fn finish_foreign(&mut self, stop: &str, call: &Call, out: &mut Outcome) {
        let text = std::mem::take(&mut self.said);
        if !text.trim().is_empty() {
            self.emit(call, out, Event::Assistant { message: Message::text("assistant", text) });
        }
        if !matches!(stop, "end_turn" | "cancelled") {
            self.emit(call, out, Event::Error { message: format!("the agent stopped: {stop}") });
        }
        let ev = Event::TurnEnd {
            steps: self.steps + 1,
            prompt: self.usage.0,
            completion: self.usage.1,
            ms: self.started.elapsed().as_millis() as u64,
            interrupted: stop == "cancelled",
            cached: self.usage.2,
            cost: None,
        };
        self.emit(call, out, ev);
    }

    fn update(&mut self, u: &Value, call: &Call, out: &mut Outcome) {
        let text = || u["content"]["text"].as_str().unwrap_or_default().to_string();
        match u["sessionUpdate"].as_str() {
            Some("agent_message_chunk") => {
                let t = text();
                self.said.push_str(&t);
                self.emit(call, out, Event::Text { text: t });
            }
            Some("agent_thought_chunk") => self.emit(call, out, Event::Reasoning { text: text() }),
            Some("tool_call") => {
                let id = u["toolCallId"].as_str().unwrap_or_default().to_string();
                self.steps += 1;
                // What it said before the call is not the answer.
                self.said.clear();
                // `name` is the tool; `title` is for display and may be a
                // status ("Preparing file…") until the input arrives.
                let name = u["name"].as_str().or(u["title"].as_str()).unwrap_or("tool").to_string();
                let args = u.get("rawInput").filter(|v| !v.is_null()).map(Value::to_string).unwrap_or_default();
                self.calls.insert(id.clone(), Tool { name, args, started: None });
                if u["status"].as_str().is_some_and(|s| s != "pending") {
                    self.start_tool(&id, call, out);
                }
            }
            Some("tool_call_update") => {
                let id = u["toolCallId"].as_str().unwrap_or_default().to_string();
                let Some(tool) = self.calls.get_mut(&id) else { return };
                if let Some(input) = u.get("rawInput").filter(|v| !v.is_null()) {
                    tool.args = input.to_string();
                }
                match u["status"].as_str() {
                    Some("in_progress") => self.start_tool(&id, call, out),
                    Some(status @ ("completed" | "failed")) => {
                        self.start_tool(&id, call, out);
                        let output: String = u["content"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|c| c["content"]["text"].as_str())
                            .collect::<Vec<_>>()
                            .join("\n");
                        let tool = self.calls.remove(&id);
                        let ms = tool.as_ref().and_then(|t| t.started).map_or(0, |t| t.elapsed().as_millis() as u64);
                        let name = tool.map(|t| t.name).unwrap_or_default();
                        let ev = Event::ToolEnd {
                            id,
                            name,
                            ok: status == "completed",
                            output,
                            ms,
                            kind: OutputKind::Plain,
                            data: None,
                            code: None,
                        };
                        self.emit(call, out, ev);
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    /// Journal the call's start once, with the input as it is now.
    fn start_tool(&mut self, id: &str, call: &Call, out: &mut Outcome) {
        let Some(tool) = self.calls.get_mut(id) else { return };
        if tool.started.is_some() {
            return;
        }
        tool.started = Some(Instant::now());
        let ev = Event::ToolStart { id: id.to_string(), name: tool.name.clone(), args: tool.args.clone() };
        self.emit(call, out, ev);
    }

    /// Answer `session/request_permission` by the definition's policy, and
    /// journal both halves. The answer is always one of the offered options.
    fn permission(&mut self, id: &Value, params: &Value, call: &Call, out: &mut Outcome) -> Vec<String> {
        let options = params["options"].as_array().cloned().unwrap_or_default();
        let kinds: Vec<String> = options.iter().filter_map(|o| o["kind"].as_str().map(str::to_string)).collect();
        let want = if self.allow { "allow" } else { "reject" };
        let pick = |prefix: &str| {
            let find = |k: String| options.iter().find(|o| o["kind"] == json!(k));
            find(format!("{prefix}_once")).or_else(|| find(format!("{prefix}_always")))
        };
        // Only the wanted kind is ever chosen: no option to say yes means no.
        let chosen = pick(want).and_then(|o| o["optionId"].as_str().map(str::to_string));
        let (outcome, decision) = match &chosen {
            Some(opt) => (json!({ "outcome": "selected", "optionId": opt }), want),
            None => (json!({ "outcome": "cancelled" }), "cancelled"),
        };
        let title = params["toolCall"]["title"].as_str().unwrap_or_default().to_string();
        let tool_call_id = params["toolCall"]["toolCallId"].as_str().unwrap_or_default().to_string();
        // The question is about a call, so the record shows the call first,
        // with the input the agent is asking about.
        if let Some(tool) = self.calls.get_mut(&tool_call_id)
            && let Some(input) = params["toolCall"].get("rawInput").filter(|v| !v.is_null())
        {
            tool.args = input.to_string();
        }
        self.start_tool(&tool_call_id, call, out);
        self.emit(call, out, Event::Permission { tool_call_id, title, options: kinds, decision: decision.into() });
        vec![rpc::ok(id, json!({ "outcome": outcome }))]
    }
}

fn step_name(id: u64) -> &'static str {
    match id {
        INIT => "initialize",
        NEW => "session/new",
        _ => "session/prompt",
    }
}

impl Wire for Client {
    fn open(&mut self) -> Vec<String> {
        let mut caps = json!({ "fs": { "readTextFile": false, "writeTextFile": false }, "terminal": false });
        if matches!(self.kind, Kind::Construct(_)) {
            caps["_meta"] = json!({ META_KEY: { "events": true } });
        }
        let params = json!({
            "protocolVersion": VERSION,
            "clientCapabilities": caps,
            "clientInfo": { "name": "construct", "title": "construct", "version": env!("CARGO_PKG_VERSION") },
        });
        vec![rpc::request(INIT, "initialize", params)]
    }

    fn line(&mut self, line: &str, call: &Call, out: &mut Outcome) -> Vec<String> {
        match rpc::parse(line) {
            Some(Incoming::Response { id, result }) => match id.as_u64() {
                Some(n @ (INIT | NEW | PROMPT)) => self.response(n, result, call, out),
                _ => vec![],
            },
            Some(Incoming::Notification { method, params }) => {
                if method == EVENT_METHOD && self.lossless {
                    if let Ok(ev) = serde_json::from_value::<Event>(params["event"].clone()) {
                        self.emit(call, out, ev);
                    }
                } else if method == "session/update" && !self.lossless {
                    self.update(&params["update"], call, out);
                }
                vec![]
            }
            Some(Incoming::Request { id, method, params }) if method == "session/request_permission" => {
                self.permission(&id, &params, call, out)
            }
            Some(Incoming::Request { id, method, .. }) => {
                vec![rpc::err(&id, rpc::METHOD_NOT_FOUND, &format!("construct does not provide {method}"))]
            }
            None => vec![],
        }
    }

    fn cancel(&mut self) -> Vec<String> {
        match &self.session {
            Some(sid) => vec![rpc::notification("session/cancel", json!({ "sessionId": sid }))],
            None => vec![],
        }
    }
}
