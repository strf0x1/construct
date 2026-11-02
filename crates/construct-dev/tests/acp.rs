//! ACP end to end: the real binary as an agent on a pipe, driven the way an
//! editor would, with the model on the scripted endpoint.

use crate::common::*;
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

/// An ACP client on the other end of `construct acp`.
pub(super) struct Client {
    child: Child,
    stdin: Option<ChildStdin>,
    out: BufReader<ChildStdout>,
    pub(super) next: u64,
    /// Responses that arrived while waiting for another id.
    pub(super) pending: std::collections::HashMap<u64, Value>,
    /// The option ids to pick, in order, when construct asks permission.
    pub(super) answers: VecDeque<&'static str>,
}

impl Client {
    pub(super) fn start(env: &Env) -> Self {
        let mut c = Command::new(construct());
        c.arg("acp").current_dir(&env.work).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        for (k, v) in env.env() {
            if v.is_empty() {
                c.env_remove(k);
            } else {
                c.env(k, v);
            }
        }
        let mut child = c.spawn().unwrap();
        let (stdin, out) = (child.stdin.take(), BufReader::new(child.stdout.take().unwrap()));
        Self { child, stdin, out, next: 0, pending: Default::default(), answers: VecDeque::new() }
    }

    pub(super) fn read(&mut self) -> Value {
        let mut line = String::new();
        assert!(self.out.read_line(&mut line).unwrap() > 0, "construct acp closed");
        serde_json::from_str(&line).unwrap()
    }

    pub(super) fn send(&mut self, v: Value) {
        let w = self.stdin.as_mut().unwrap();
        writeln!(w, "{v}").unwrap();
        w.flush().unwrap();
    }

    pub(super) fn notify(&mut self, method: &str, params: Value) {
        self.send(json!({ "jsonrpc": "2.0", "method": method, "params": params }));
    }

    /// Send a request and return its id, for a response that may arrive after
    /// another one (a cancelled prompt and the close that cancelled it).
    pub(super) fn request(&mut self, method: &str, params: Value) -> u64 {
        self.next += 1;
        let id = self.next;
        self.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        id
    }

    /// Send a request; collect the notifications (and permission requests,
    /// answered from `answers`) that arrive before its response.
    pub(super) fn call(&mut self, method: &str, params: Value) -> (Value, Vec<Value>) {
        let id = self.request(method, params);
        self.await_response(id)
    }

    pub(super) fn await_response(&mut self, id: u64) -> (Value, Vec<Value>) {
        if let Some(v) = self.pending.remove(&id) {
            return (v, vec![]);
        }
        let mut notes = vec![];
        loop {
            let mut l = String::new();
            assert!(self.out.read_line(&mut l).unwrap() > 0, "construct acp closed its output");
            let v: Value = serde_json::from_str(&l).unwrap_or_else(|e| panic!("not JSON ({e}): {l}"));
            assert_eq!(v["jsonrpc"], "2.0", "{l}");
            if v["id"] == json!(id) && v["method"].is_null() {
                return (v, notes);
            }
            if v["method"] == "session/request_permission" {
                let pick = self.answers.pop_front().expect("an unexpected permission request");
                let outcome = json!({ "outcome": "selected", "optionId": pick });
                self.send(json!({ "jsonrpc": "2.0", "id": v["id"], "result": { "outcome": outcome } }));
            }
            if v["method"].is_null()
                && let Some(other) = v["id"].as_u64()
            {
                self.pending.insert(other, v.clone());
            }
            notes.push(v);
        }
    }

    /// Stop the agent the way a crash or a stopped VM does: no shutdown, no
    /// closing the journal by hand.
    fn kill(mut self) {
        self.child.kill().unwrap();
        let _ = self.child.wait();
    }

    pub(super) fn finish(mut self) {
        drop(self.stdin.take());
        assert!(self.child.wait().unwrap().success());
    }
}

pub(super) fn updates<'a>(notes: &'a [Value], kind: &str) -> Vec<&'a Value> {
    notes
        .iter()
        .filter(|n| n["method"] == "session/update" && n["params"]["update"]["sessionUpdate"] == kind)
        .map(|n| &n["params"]["update"])
        .collect()
}

#[test]
fn an_acp_client_initializes_prompts_and_gets_the_streamed_turn() {
    let env = Env::new(
        "acp-smoke",
        json!({ "steps": [
            { "call": "read", "args": { "path": "note.txt" } },
            { "text": "it says hello" }
        ] }),
    );
    std::fs::write(env.work.join("note.txt"), "hello\n").unwrap();
    let mut c = Client::start(&env);

    let (init, _) = c.call("initialize", json!({ "protocolVersion": 1, "clientCapabilities": {} }));
    assert_eq!(init["result"]["protocolVersion"], 1);
    assert_eq!(init["result"]["agentInfo"]["name"], "construct");
    assert_eq!(init["result"]["authMethods"], json!([]));

    let (new, _) = c.call("session/new", json!({ "cwd": env.work.display().to_string(), "mcpServers": [] }));
    let sid = new["result"]["sessionId"].as_str().unwrap().to_string();

    let (done, notes) =
        c.call("session/prompt", json!({ "sessionId": sid, "prompt": [{ "type": "text", "text": "read note.txt" }] }));
    assert_eq!(done["result"]["stopReason"], "end_turn", "{done}");
    assert!(notes.iter().all(|n| n["params"]["sessionId"] == json!(sid)));
    let call = updates(&notes, "tool_call");
    assert_eq!((call.len(), call[0]["status"].as_str()), (1, Some("in_progress")));
    // ACP's fixed category, not the tool's name, so an editor picks the right icon.
    assert_eq!((call[0]["kind"].as_str(), call[0]["title"].as_str()), (Some("read"), Some("read note.txt")));
    let end = updates(&notes, "tool_call_update");
    assert_eq!(
        (end[0]["status"].as_str(), end[0]["toolCallId"].as_str()),
        (Some("completed"), call[0]["toolCallId"].as_str())
    );
    let said: String =
        updates(&notes, "agent_message_chunk").iter().filter_map(|u| u["content"]["text"].as_str()).collect();
    assert_eq!(said, "it says hello");

    // The wire's session id is the journal's.
    assert_eq!(env.sessions(), vec![sid.clone()]);

    let (bad, _) = c.call("session/prompt", json!({ "sessionId": "nope", "prompt": [] }));
    assert_eq!(bad["error"]["code"], -32602);
    let (unknown, _) = c.call("session/frobnicate", json!({}));
    assert_eq!(unknown["error"]["code"], -32601);
    c.finish();
}

#[test]
fn a_completed_turn_is_on_disk_after_a_kill_without_close() {
    // #161: every row the client saw is journaled when it sees it. This kills
    // the process, so the host page cache survives; a VM/host death also needs
    // the directory syncs in `Journal::open` and the first commit (#163),
    // which no in-process test can drop.
    let env = Env::new("acp-durable", json!({ "steps": [{ "text": "written before the kill" }] }));
    env.config("[router]\nmode = \"local\"\n");
    let mut c = Client::start(&env);
    c.call("initialize", json!({ "protocolVersion": 1 }));
    let (new, _) = c.call("session/new", json!({ "cwd": env.work.display().to_string(), "mcpServers": [] }));
    let sid = new["result"]["sessionId"].as_str().unwrap().to_string();
    let (done, notes) =
        c.call("session/prompt", json!({ "sessionId": sid, "prompt": [{ "type": "text", "text": "write it" }] }));
    assert_eq!(done["result"]["stopReason"], "end_turn");
    let said: String =
        updates(&notes, "agent_message_chunk").iter().filter_map(|u| u["content"]["text"].as_str()).collect();
    // SIGKILL, not `session/close`: the crash or VM death in the issue.
    c.kill();
    assert_eq!(env.sessions(), vec![sid.clone()]);
    let rows = env.journal();
    let types: Vec<&str> = rows.iter().map(|r| r.ty.as_str()).collect();
    assert!(types.contains(&"turn_end"), "{types:?}");
    let assistant = rows.iter().find(|r| r.ty == "assistant").expect("the final message was journaled");
    assert_eq!(assistant.payload["message"]["content"], json!(said));
}

#[test]
fn a_blocked_path_is_asked_of_the_client_and_its_answer_applies() {
    let outside = Env::dir_for("acp-ask").join("outside").join("secret.txt");
    let read = json!({ "call": "read", "args": { "path": outside.display().to_string() } });
    let env = Env::new("acp-ask", json!({ "steps": [read.clone(), read, { "text": "done" }] }));
    std::fs::create_dir_all(outside.parent().unwrap()).unwrap();
    std::fs::write(&outside, "hidden\n").unwrap();
    let mut c = Client::start(&env);
    c.call("initialize", json!({ "protocolVersion": 1 }));
    let (new, _) = c.call("session/new", json!({ "cwd": env.work.display().to_string() }));
    let sid = new["result"]["sessionId"].as_str().unwrap().to_string();

    // Once lets the first read through; the second asks again and is denied.
    c.answers = VecDeque::from(["once", "deny"]);
    let (done, notes) =
        c.call("session/prompt", json!({ "sessionId": sid, "prompt": [{ "type": "text", "text": "read it" }] }));
    assert_eq!(done["result"]["stopReason"], "end_turn", "{done}");
    let asks: Vec<&Value> = notes.iter().filter(|n| n["method"] == "session/request_permission").collect();
    assert_eq!(asks.len(), 2, "{asks:?}");
    let p = &asks[0]["params"];
    assert_eq!((p["sessionId"].as_str(), p["toolCall"]["kind"].as_str()), (Some(sid.as_str()), Some("read")));
    assert!(p["toolCall"]["title"].as_str().unwrap().contains("secret.txt"), "{p}");
    let offered: Vec<&str> = p["options"].as_array().unwrap().iter().filter_map(|o| o["optionId"].as_str()).collect();
    for want in ["once", "session", "always", "deny"] {
        assert!(offered.contains(&want), "{want} missing from {offered:?}");
    }
    let ends: Vec<&str> = updates(&notes, "tool_call_update").iter().filter_map(|u| u["status"].as_str()).collect();
    assert_eq!(ends, ["completed", "failed"]);
    c.finish();
}

#[test]
fn a_client_that_asks_gets_every_event_whole() {
    let env = Env::new("acp-lossless", json!({ "steps": [{ "text": "hi" }] }));
    let mut c = Client::start(&env);
    let caps = json!({ "_meta": { "construct": { "events": true } } });
    let (init, _) = c.call("initialize", json!({ "protocolVersion": 1, "clientCapabilities": caps }));
    assert_eq!(init["result"]["agentCapabilities"]["_meta"]["construct"]["events"], true);
    let (new, _) = c.call("session/new", json!({ "cwd": env.work.display().to_string() }));
    let sid = new["result"]["sessionId"].as_str().unwrap().to_string();
    let (_, notes) =
        c.call("session/prompt", json!({ "sessionId": sid, "prompt": [{ "type": "text", "text": "go" }] }));
    let kinds: Vec<&str> = notes
        .iter()
        .filter(|n| n["method"] == "_construct/event")
        .filter_map(|n| n["params"]["event"]["event"].as_str())
        .collect();
    for k in ["head", "system", "user", "request", "assistant", "usage", "turn_end"] {
        assert!(kinds.contains(&k), "{k} missing from {kinds:?}");
    }
    assert!(notes.iter().all(|n| n["method"] != "session/update"), "no duplicate lossy stream");
    c.finish();
}

#[test]
fn session_cancel_ends_a_running_turn_as_cancelled() {
    let env = Env::new(
        "acp-cancel",
        json!({ "steps": [{ "call": "shell", "args": { "command": "sleep 30" } }, { "text": "late" }] }),
    );
    let mut c = Client::start(&env);
    c.call("initialize", json!({ "protocolVersion": 1 }));
    let (new, _) = c.call("session/new", json!({ "cwd": env.work.display().to_string() }));
    let sid = new["result"]["sessionId"].as_str().unwrap().to_string();
    c.send(json!({ "jsonrpc": "2.0", "id": 99, "method": "session/prompt",
        "params": { "sessionId": sid, "prompt": [{ "type": "text", "text": "wait" }] } }));
    // Cancel once the tool is running.
    loop {
        let mut l = String::new();
        c.out.read_line(&mut l).unwrap();
        if l.contains("\"tool_call\"") {
            break;
        }
    }
    c.notify("session/cancel", json!({ "sessionId": sid }));
    let started = std::time::Instant::now();
    let resp = loop {
        let mut l = String::new();
        assert!(c.out.read_line(&mut l).unwrap() > 0);
        let v: Value = serde_json::from_str(&l).unwrap();
        if v["id"] == json!(99) {
            break v;
        }
    };
    assert_eq!(resp["result"]["stopReason"], "cancelled", "{resp}");
    assert!(started.elapsed().as_millis() < budget(10_000) as u128);
    c.finish();
}

#[test]
fn session_load_capabilities_and_refusals_are_clear() {
    let env = Env::new("acp-load-refusals", json!({ "steps": [{ "text": "hi" }] }));
    let mut first = Client::start(&env);
    let (init, _) = first.call("initialize", json!({ "protocolVersion": 1 }));
    let caps = &init["result"]["agentCapabilities"];
    assert_eq!(caps["loadSession"], true);
    assert_eq!(caps["_meta"]["construct"]["liveAttach"], true);
    assert_eq!(caps["sessionCapabilities"], json!({ "resume": {}, "close": {} }));
    let (new, _) = first.call("session/new", json!({ "cwd": env.work.display().to_string(), "mcpServers": [] }));
    let sid = new["result"]["sessionId"].as_str().unwrap().to_string();
    first.finish();

    let mut c = Client::start(&env);
    c.call("initialize", json!({ "protocolVersion": 1 }));
    // Unknown: ACP's resource-not-found, naming the session.
    let (none, _) =
        c.call("session/load", json!({ "sessionId": "nope", "cwd": env.work.display().to_string(), "mcpServers": [] }));
    assert_eq!(none["error"]["code"], -32002, "{none}");
    assert_eq!(none["error"]["data"]["sessionId"], "nope");
    assert!(none["error"]["message"].as_str().unwrap().contains("no session"), "{none}");

    // Another directory: the journal knows the session, this cwd does not.
    let elsewhere = env.dir.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let (wrong, _) =
        c.call("session/load", json!({ "sessionId": sid, "cwd": elsewhere.display().to_string(), "mcpServers": [] }));
    assert_eq!(wrong["error"]["code"], -32602, "{wrong}");
    assert!(wrong["error"]["message"].as_str().unwrap().contains("belongs to"), "{wrong}");

    // A second agent on one journal is refused.
    let (loaded, _) =
        c.call("session/load", json!({ "sessionId": sid, "cwd": env.work.display().to_string(), "mcpServers": [] }));
    assert_eq!(loaded["result"]["_meta"]["construct"]["turn"]["state"], "idle", "{loaded}");
    let (again, _) =
        c.call("session/load", json!({ "sessionId": sid, "cwd": env.work.display().to_string(), "mcpServers": [] }));
    assert_eq!(again["error"]["code"], -32602, "{again}");
    assert!(again["error"]["message"].as_str().unwrap().contains("already loaded"), "{again}");
    c.finish();
}

#[test]
fn a_session_owned_by_another_process_is_refused() {
    let env = Env::new("acp-load-owned", json!({ "steps": [{ "text": "hi" }] }));
    let mut first = Client::start(&env);
    first.call("initialize", json!({ "protocolVersion": 1 }));
    let (new, _) = first.call("session/new", json!({ "cwd": env.work.display().to_string() }));
    let sid = new["result"]["sessionId"].as_str().unwrap().to_string();

    let mut second = Client::start(&env);
    second.call("initialize", json!({ "protocolVersion": 1 }));
    let (busy, _) = second
        .call("session/load", json!({ "sessionId": sid, "cwd": env.work.display().to_string(), "mcpServers": [] }));
    assert_eq!(busy["error"]["code"], -32602, "{busy}");
    assert!(busy["error"]["message"].as_str().unwrap().contains("in use by another process"), "{busy}");
    second.finish();
    first.finish();
}

#[test]
fn a_killed_session_loads_its_history_and_the_unknown_tool_is_not_rerun() {
    let env = Env::new(
        "acp-load",
        json!({ "steps": [
            { "call": "shell", "args": { "command": "sleep 30" } },
            { "text": "back" }
        ] }),
    );
    let mut c = Client::start(&env);
    c.call("initialize", json!({ "protocolVersion": 1 }));
    let (new, _) = c.call("session/new", json!({ "cwd": env.work.display().to_string(), "mcpServers": [] }));
    let sid = new["result"]["sessionId"].as_str().unwrap().to_string();
    c.request("session/prompt", json!({ "sessionId": sid, "prompt": [{ "type": "text", "text": "wait for it" }] }));
    // Kill the agent once the tool is running, as a crash or a stopped VM would.
    loop {
        let mut l = String::new();
        assert!(c.out.read_line(&mut l).unwrap() > 0);
        if l.contains("\"tool_call\"") {
            break;
        }
    }
    c.kill();

    let mut c = Client::start(&env);
    c.call("initialize", json!({ "protocolVersion": 1 }));
    let (loaded, notes) =
        c.call("session/load", json!({ "sessionId": sid, "cwd": env.work.display().to_string(), "mcpServers": [] }));
    assert_eq!(loaded["result"]["_meta"]["construct"]["turn"]["state"], "idle", "{loaded}");
    assert!(notes.iter().all(|n| n["params"]["sessionId"] == json!(sid)));
    let users: String =
        updates(&notes, "user_message_chunk").iter().filter_map(|u| u["content"]["text"].as_str()).collect();
    assert_eq!(users, "wait for it");
    let calls = updates(&notes, "tool_call");
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert_eq!((calls[0]["status"].as_str(), calls[0]["kind"].as_str()), (Some("failed"), Some("execute")));
    let output = calls[0]["content"][0]["content"]["text"].as_str().unwrap_or_default();
    assert!(output.contains("outcome is unknown"), "{output}");
    let said: String =
        updates(&notes, "agent_message_chunk").iter().filter_map(|u| u["content"]["text"].as_str()).collect();
    assert!(said.contains("Previous turn was interrupted"), "{said}");

    // The session continues, and the interrupted call was never retried.
    let (done, notes) =
        c.call("session/prompt", json!({ "sessionId": sid, "prompt": [{ "type": "text", "text": "again" }] }));
    assert_eq!(done["result"]["stopReason"], "end_turn", "{done}");
    let said: String =
        updates(&notes, "agent_message_chunk").iter().filter_map(|u| u["content"]["text"].as_str()).collect();
    assert_eq!(said, "back");
    let rows = env.journal();
    let starts = rows.iter().filter(|r| r.session == sid && r.ty == "tool_start").count();
    assert_eq!(starts, 1, "the unknown call was not re-run");
    let ends: Vec<&Value> =
        rows.iter().filter(|r| r.session == sid && r.ty == "tool_end").map(|r| &r.payload).collect();
    assert_eq!(ends.len(), 1, "{ends:?}");
    assert_eq!(ends[0]["code"], "interrupted_unknown");
    c.finish();
}

#[test]
fn session_resume_reattaches_without_history_and_can_prompt() {
    let env = Env::new("acp-resume", json!({ "steps": [{ "text": "first" }, { "text": "second" }] }));
    let mut c = Client::start(&env);
    c.call("initialize", json!({ "protocolVersion": 1 }));
    let (new, _) = c.call("session/new", json!({ "cwd": env.work.display().to_string(), "mcpServers": [] }));
    let sid = new["result"]["sessionId"].as_str().unwrap().to_string();
    let (done, _) =
        c.call("session/prompt", json!({ "sessionId": sid, "prompt": [{ "type": "text", "text": "one" }] }));
    assert_eq!(done["result"]["stopReason"], "end_turn", "{done}");
    c.finish();

    let mut c = Client::start(&env);
    let (init, _) = c.call("initialize", json!({ "protocolVersion": 1 }));
    assert_eq!(init["result"]["agentCapabilities"]["sessionCapabilities"]["resume"], json!({}));
    let (resumed, notes) =
        c.call("session/resume", json!({ "sessionId": sid, "cwd": env.work.display().to_string(), "mcpServers": [] }));
    assert_eq!(resumed["result"], json!({}), "{resumed}");
    assert!(notes.is_empty(), "resume replays nothing: {notes:?}");
    let (done, notes) =
        c.call("session/prompt", json!({ "sessionId": sid, "prompt": [{ "type": "text", "text": "two" }] }));
    assert_eq!(done["result"]["stopReason"], "end_turn", "{done}");
    let said: String =
        updates(&notes, "agent_message_chunk").iter().filter_map(|u| u["content"]["text"].as_str()).collect();
    assert_eq!(said, "second");
    c.finish();
}

#[test]
fn session_close_ends_the_turn_and_is_idempotent() {
    let env = Env::new(
        "acp-close",
        json!({ "steps": [{ "call": "shell", "args": { "command": "sleep 30" } }, { "text": "late" }] }),
    );
    let mut c = Client::start(&env);
    c.call("initialize", json!({ "protocolVersion": 1 }));
    let (new, _) = c.call("session/new", json!({ "cwd": env.work.display().to_string(), "mcpServers": [] }));
    let sid = new["result"]["sessionId"].as_str().unwrap().to_string();
    let prompt =
        c.request("session/prompt", json!({ "sessionId": sid, "prompt": [{ "type": "text", "text": "wait" }] }));
    loop {
        let mut l = String::new();
        assert!(c.out.read_line(&mut l).unwrap() > 0);
        if l.contains("\"tool_call\"") {
            break;
        }
    }
    let started = std::time::Instant::now();
    let (closed, _) = c.call("session/close", json!({ "sessionId": sid }));
    assert_eq!(closed["result"], json!({}), "{closed}");
    let (done, _) = c.await_response(prompt);
    assert_eq!(done["result"]["stopReason"], "cancelled", "{done}");
    assert!(started.elapsed().as_millis() < budget(10_000) as u128);
    // Closing again, or closing what was never open, still succeeds.
    let (again, _) = c.call("session/close", json!({ "sessionId": sid }));
    assert_eq!(again["result"], json!({}), "{again}");
    let (unknown, _) = c.call("session/close", json!({ "sessionId": "nope" }));
    assert_eq!(unknown["result"], json!({}), "{unknown}");
    c.finish();
}
