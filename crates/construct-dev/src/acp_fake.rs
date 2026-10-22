//! `construct-dev acp-fake`: a scripted ACP agent on stdio, standing in for
//! codex, claude, gemini or opencode in tests. One `session/prompt` plays a
//! fixed stream: a message, a tool call, a `session/request_permission` the
//! client must answer, the tool's result naming the option it chose, and a
//! closing message. `--tail` reports how a `session/cancel` is handled.
//!
//! The design choice: like `mcp-fake` it is synchronous and answers one line
//! at a time, so a test controls exactly what the client sees and in what
//! order. It echoes what it received (the prompt, the answer) into the
//! stream, which is how a test observes the client's half.

use anyhow::Result;
use serde_json::{Value, json};
use std::io::{BufRead, Write};

#[derive(clap::Args, Debug, Clone, Default)]
pub struct Opts {
    /// Sleep after the permission answer until `session/cancel` arrives, then
    /// end the turn as cancelled.
    #[arg(long)]
    wait_for_cancel: bool,
    /// Offer only these option kinds (comma-separated) instead of all four.
    #[arg(long)]
    kinds: Option<String>,
    /// Report this environment variable's value in the closing message.
    #[arg(long)]
    report_env: Option<String>,
    /// Keep running after the turn ends, ignoring stdin closing, as some real
    /// adapters (codex-acp) do.
    #[arg(long)]
    linger: bool,
    /// Ask the client for a file it didn't offer, and report the error.
    #[arg(long)]
    ask_fs: bool,
}

fn send(v: Value) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{v}");
    let _ = out.flush();
}

fn update(sid: &str, u: Value) {
    send(json!({ "jsonrpc": "2.0", "method": "session/update", "params": { "sessionId": sid, "update": u } }));
}

fn text(sid: &str, t: &str) {
    update(sid, json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": t } }));
}

pub fn run(o: Opts) -> Result<()> {
    let mut lines = std::io::stdin().lock().lines();
    let mut next_line = move || lines.next().and_then(Result::ok).and_then(|l| serde_json::from_str::<Value>(&l).ok());
    while let Some(m) = next_line() {
        let id = m["id"].clone();
        match m["method"].as_str() {
            Some("initialize") => send(json!({ "jsonrpc": "2.0", "id": id, "result": {
                "protocolVersion": 1,
                "agentInfo": { "name": "acp-fake", "version": "9.9" },
                "agentCapabilities": { "loadSession": false },
                "authMethods": [] } })),
            Some("session/new") => {
                send(json!({ "jsonrpc": "2.0", "id": id, "result": { "sessionId": "fake-session-1" } }))
            }
            Some("session/prompt") => {
                let sid = m["params"]["sessionId"].as_str().unwrap_or("").to_string();
                let prompt = m["params"]["prompt"][0]["text"].as_str().unwrap_or("");
                text(&sid, &format!("working on: {prompt}"));
                // As real adapters do: the call opens pending with the input so
                // far, and the real input arrives when it starts.
                update(
                    &sid,
                    json!({ "sessionUpdate": "tool_call", "toolCallId": "t1", "name": "bash", "title": "Preparing command…",
                    "kind": "execute", "status": "pending", "rawInput": { "cwd": "/w" } }),
                );
                update(
                    &sid,
                    json!({ "sessionUpdate": "tool_call_update", "toolCallId": "t1", "title": "rm -rf build",
                    "status": "in_progress", "rawInput": { "command": "rm -rf build", "cwd": "/w" } }),
                );
                let all = ["allow_once", "allow_always", "reject_once", "reject_always"];
                let kinds: Vec<&str> = match &o.kinds {
                    Some(k) => k.split(',').collect(),
                    None => all.to_vec(),
                };
                let options: Vec<Value> =
                    kinds.iter().map(|k| json!({ "optionId": format!("opt-{k}"), "name": k, "kind": k })).collect();
                send(json!({ "jsonrpc": "2.0", "id": "perm-1", "method": "session/request_permission", "params": {
                    "sessionId": sid,
                    "toolCall": { "toolCallId": "t1", "title": "rm -rf build" },
                    "options": options } }));
                if o.ask_fs {
                    send(json!({ "jsonrpc": "2.0", "id": "fs-1", "method": "fs/read_text_file",
                        "params": { "sessionId": sid, "path": "/etc/hostname" } }));
                }
                let (mut chosen, mut fs_err) = (None, None);
                while chosen.is_none() || (o.ask_fs && fs_err.is_none()) {
                    let Some(r) = next_line() else { return Ok(()) };
                    match r["id"].as_str() {
                        Some("perm-1") => chosen = Some(r["result"]["outcome"].clone()),
                        Some("fs-1") => fs_err = Some(r["error"]["code"].clone()),
                        _ => {}
                    }
                }
                let chosen = chosen.unwrap_or_default();
                let answer = match chosen["outcome"].as_str() {
                    Some("selected") => chosen["optionId"].as_str().unwrap_or("?").to_string(),
                    other => other.unwrap_or("none").to_string(),
                };
                update(
                    &sid,
                    json!({ "sessionUpdate": "tool_call_update", "toolCallId": "t1",
                    "status": if answer.contains("allow") { "completed" } else { "failed" },
                    "content": [{ "type": "content", "content": { "type": "text", "text": format!("permission: {answer}") } }] }),
                );
                if let Some(code) = fs_err {
                    text(&sid, &format!("fs error {code}. "));
                }
                if o.wait_for_cancel {
                    while let Some(r) = next_line() {
                        if r["method"] == "session/cancel" {
                            send(json!({ "jsonrpc": "2.0", "id": id, "result": { "stopReason": "cancelled" } }));
                            break;
                        }
                    }
                    continue;
                }
                let env = o.report_env.as_ref().map(|n| format!(" {n}={}", std::env::var(n).unwrap_or_default()));
                text(&sid, &format!("done: {answer}{}", env.unwrap_or_default()));
                send(json!({ "jsonrpc": "2.0", "id": id, "result": { "stopReason": "end_turn",
                    "usage": { "inputTokens": 11, "outputTokens": 7, "cachedReadTokens": 5 } } }));
                if o.linger {
                    std::thread::sleep(std::time::Duration::from_secs(120));
                }
            }
            Some(_) if !id.is_null() => {
                send(json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": "unknown" } }))
            }
            _ => {}
        }
    }
    Ok(())
}
