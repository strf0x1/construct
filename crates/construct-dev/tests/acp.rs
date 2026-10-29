//! ACP end to end: the real binary as an agent on a pipe, driven the way an
//! editor would, with the model on the scripted endpoint.

use crate::common::*;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

/// An ACP client on the other end of `construct acp`.
struct Client {
    child: Child,
    stdin: Option<ChildStdin>,
    out: BufReader<ChildStdout>,
    next: u64,
}

impl Client {
    fn start(env: &Env) -> Self {
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
        Self { child, stdin, out, next: 0 }
    }

    fn send(&mut self, v: Value) {
        let w = self.stdin.as_mut().unwrap();
        writeln!(w, "{v}").unwrap();
        w.flush().unwrap();
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.send(json!({ "jsonrpc": "2.0", "method": method, "params": params }));
    }

    /// Send a request; collect the notifications that arrive before its response.
    fn call(&mut self, method: &str, params: Value) -> (Value, Vec<Value>) {
        self.next += 1;
        let id = self.next;
        self.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        let mut notes = vec![];
        loop {
            let mut l = String::new();
            assert!(self.out.read_line(&mut l).unwrap() > 0, "construct acp closed its output");
            let v: Value = serde_json::from_str(&l).unwrap_or_else(|e| panic!("not JSON ({e}): {l}"));
            assert_eq!(v["jsonrpc"], "2.0", "{l}");
            if v["id"] == json!(id) {
                return (v, notes);
            }
            notes.push(v);
        }
    }

    fn finish(mut self) {
        drop(self.stdin.take());
        assert!(self.child.wait().unwrap().success());
    }
}

fn updates<'a>(notes: &'a [Value], kind: &str) -> Vec<&'a Value> {
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
