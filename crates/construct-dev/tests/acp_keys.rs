//! Idempotent prompts over ACP (contract 1.3.0 §12, #245): a prompt resent
//! with its key starts no second turn, whether the first is queued, running,
//! finished, or was cut off by a kill. The journal holds exactly one
//! `submitted` row per key.

use super::acp::Client;
use crate::common::*;
use serde_json::{Value, json};

const META: &str = "com.sprawlcommons/construct";

fn prompt(sid: &str, text: &str, key: Value) -> Value {
    json!({ "sessionId": sid, "prompt": [{ "type": "text", "text": text }], "_meta": { META: { "idempotencyKey": key } } })
}

fn open(c: &mut Client, env: &Env) -> String {
    let (new, _) = c.call("session/new", json!({ "cwd": env.work.display().to_string(), "mcpServers": [] }));
    new["result"]["sessionId"].as_str().unwrap().to_string()
}

fn submitted(env: &Env) -> Vec<Value> {
    env.journal().into_iter().filter(|r| r.ty == "submitted").map(|r| r.payload).collect()
}

fn agent_requests(env: &Env) -> usize {
    env.requests().iter().filter(|r| r["tools"].as_array().is_some_and(|t| !t.is_empty())).count()
}

#[test]
fn a_prompt_resent_after_a_kill_is_answered_cancelled_and_never_runs_again() {
    let env = Env::new(
        "acp-keys-kill",
        json!({ "steps": [
            { "call": "shell", "args": { "command": "echo ran >> effects.txt; sleep 30" } },
            { "text": "a new prompt ran" }
        ] }),
    );
    env.config("[router]\nmode = \"local\"\n[learn]\nenabled = false\n");
    let mut c = Client::start(&env);
    c.call("initialize", json!({ "protocolVersion": 1 }));
    let sid = open(&mut c, &env);
    c.request("session/prompt", prompt(&sid, "migrate the database", json!("key-1")));
    c.until("\"tool_call\"");
    // The call is announced before the shell runs; kill once its effect is real.
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(budget(5000));
    while !env.work.join("effects.txt").exists() {
        assert!(std::time::Instant::now() < deadline, "the shell never ran");
        std::thread::sleep(std::time::Duration::from_millis(15));
    }
    c.kill();
    let sent = agent_requests(&env);

    let mut c = Client::start(&env);
    let (init, _) = c.call("initialize", json!({ "protocolVersion": 1 }));
    let meta = &init["result"]["agentCapabilities"]["_meta"];
    assert_eq!(meta[META]["idempotentPrompts"], true, "{meta}");
    assert!(meta["construct"]["idempotentPrompts"].is_null(), "no legacy form: {meta}");
    c.call("session/load", json!({ "sessionId": sid, "cwd": env.work.display().to_string(), "mcpServers": [] }));

    let (again, _) = c.call("session/prompt", prompt(&sid, "migrate the database", json!("key-1")));
    assert_eq!(again["result"]["stopReason"], "cancelled", "{again}");
    assert_eq!(agent_requests(&env), sent, "the resend reached no model");
    assert_eq!(std::fs::read_to_string(env.work.join("effects.txt")).unwrap(), "ran\n");
    let rows = submitted(&env);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["idempotency_key"], "key-1");

    // A new key is a new prompt and runs.
    let (next, _) = c.call("session/prompt", prompt(&sid, "now something else", json!("key-2")));
    assert_eq!(next["result"]["stopReason"], "end_turn", "{next}");
    assert_eq!(submitted(&env).len(), 2);
    c.finish();
}

#[test]
fn resends_wait_for_a_queued_or_running_prompt_and_a_finished_one_answers_at_once() {
    let env = Env::new(
        "acp-keys-live",
        json!({ "steps": [
            { "call": "shell", "args": { "command": "sleep 1; printf slept" } },
            { "text": "first done" },
            { "text": "second done" }
        ] }),
    );
    env.config("[router]\nmode = \"local\"\n[learn]\nenabled = false\n");
    let mut c = Client::start(&env);
    // Ask for the lossless channel: it carries journal events, which must not carry keys.
    c.call(
        "initialize",
        json!({ "protocolVersion": 1, "clientCapabilities": { "_meta": { META: { "events": true } } } }),
    );
    let sid = open(&mut c, &env);

    let first = c.request("session/prompt", prompt(&sid, "first", json!("k-first")));
    c.until("\"tool_start\"");
    let first_again = c.request("session/prompt", prompt(&sid, "first", json!("k-first")));
    let second = c.request("session/prompt", prompt(&sid, "second", json!("k-second")));
    let second_again = c.request("session/prompt", prompt(&sid, "second", json!("k-second")));

    let (a, mut notes) = c.await_response(first);
    let (b, more) = c.await_response(first_again);
    notes.extend(more);
    assert_eq!(
        (a["result"]["stopReason"].as_str(), b["result"]["stopReason"].as_str()),
        (Some("end_turn"), Some("end_turn"))
    );
    let (c2, more) = c.await_response(second);
    notes.extend(more);
    let (d, more) = c.await_response(second_again);
    notes.extend(more);
    assert_eq!(
        (c2["result"]["stopReason"].as_str(), d["result"]["stopReason"].as_str()),
        (Some("end_turn"), Some("end_turn"))
    );
    assert_eq!(submitted(&env).len(), 2, "one row per key");
    assert_eq!(env.journal().iter().filter(|r| r.ty == "turn_start").count(), 2, "one turn per key");
    let sent = agent_requests(&env);

    // A resend of a finished prompt is answered from the record.
    let (late, more) = c.call("session/prompt", prompt(&sid, "second", json!("k-second")));
    notes.extend(more);
    assert_eq!(late["result"]["stopReason"], "end_turn", "{late}");
    assert_eq!(agent_requests(&env), sent);

    // The same key with another prompt is refused, and the key keeps its first prompt.
    let (clash, _) = c.call("session/prompt", prompt(&sid, "something else", json!("k-second")));
    assert_eq!(clash["error"]["code"], -32602, "{clash}");
    assert_eq!(clash["error"]["data"]["reason"], "idempotency_key_conflict", "{clash}");

    // A malformed key is refused, never run as a prompt without one.
    for bad in [json!(""), json!("k".repeat(256)), json!(7)] {
        let (refused, _) = c.call("session/prompt", prompt(&sid, "bad key", bad.clone()));
        assert_eq!(refused["error"]["code"], -32602, "{bad}: {refused}");
    }
    assert_eq!(submitted(&env).len(), 2, "nothing refused was journaled");
    assert_eq!(agent_requests(&env), sent);

    let wire = notes.iter().map(Value::to_string).collect::<String>();
    assert!(wire.contains("\"submitted\""), "the lossless channel carried the submission");
    assert!(!wire.contains("k-first") && !wire.contains("k-second"), "keys stay off the wire");
    c.finish();
}
