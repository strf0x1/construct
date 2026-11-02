//! Live ACP attachments against one real process. Replay replaces the view;
//! the continuation and turn notification must complete it exactly once.

use crate::acp::{Client, updates};
use crate::common::*;
use serde_json::{Value, json};

fn start(env: &Env) -> (Client, String) {
    let mut client = Client::start(env);
    client.call("initialize", json!({ "protocolVersion": 1 }));
    let (new, _) = client.call("session/new", json!({ "cwd": env.work.display().to_string() }));
    (client, new["result"]["sessionId"].as_str().unwrap().to_string())
}

fn prompt(client: &mut Client, sid: &str) -> u64 {
    client.request("session/prompt", json!({ "sessionId": sid, "prompt": [{ "type": "text", "text": "go" }] }))
}

fn load(client: &mut Client, env: &Env, sid: &str) -> (Value, Vec<Value>) {
    client.call("session/load", json!({ "sessionId": sid, "cwd": env.work.display().to_string() }))
}

fn text(notes: &[Value]) -> String {
    updates(notes, "agent_message_chunk").iter().filter_map(|u| u["content"]["text"].as_str()).collect()
}

fn idle(client: &mut Client, sid: &str) -> Vec<Value> {
    let mut notes = vec![];
    loop {
        let next = client.read();
        let done = next["method"] == "_construct/turn"
            && next["params"]["sessionId"] == sid
            && next["params"]["state"] == "idle";
        assert!(!next["method"].is_null(), "an old prompt response crossed attachments: {next}");
        notes.push(next);
        if done {
            return notes;
        }
    }
}

#[test]
fn reconnect_streaming_replays_partial_then_continues_without_gaps_or_duplicates() {
    let answer = "zero one two three four five six seven eight nine ten eleven twelve thirteen fourteen fifteen";
    let env = Env::new("acp-live-stream", json!({ "chunk_ms": 30, "steps": [{ "text": answer }] }));
    let (mut c, sid) = start(&env);
    let old_prompt = prompt(&mut c, &sid);
    loop {
        if c.read()["params"]["update"]["sessionUpdate"] == "agent_message_chunk" {
            break;
        }
    }
    // The reopened client reuses the original prompt's id for initialize.
    c.next = old_prompt - 1;
    let (init, _) = c.call("initialize", json!({ "protocolVersion": 1 }));
    assert_eq!(init["result"]["agentCapabilities"]["_meta"]["construct"]["liveAttach"], true);
    let (attached, replay) = load(&mut c, &env, &sid);
    assert_eq!(
        attached["result"]["_meta"]["construct"]["turn"],
        json!({ "sessionId": sid, "turn": 1, "state": "working" })
    );
    assert!(!text(&replay).is_empty(), "partial text is replayed: {replay:?}");
    assert!(answer.starts_with(&text(&replay)));
    let later = idle(&mut c, &sid);
    assert_eq!(text(&replay) + &text(&later), answer);
    assert_eq!(later.last().unwrap()["params"]["stopReason"], "end_turn");
    // A barrier after completion detects even an old response queued after idle.
    let (_, trailing) = c.call("unknown/barrier", json!({}));
    assert!(trailing.iter().all(|v| !v["method"].is_null()), "{trailing:?}");
    assert!(c.pending.is_empty(), "{:#?}", c.pending);
    assert_eq!(env.requests().len(), 1, "attach never starts another model call");
    assert!(env.journal().iter().all(|row| row.ty != "recovered"));
    c.finish();
}

#[test]
fn reconnect_reissues_permission_and_retires_old_ids_at_initialize() {
    let outside = Env::dir_for("acp-live-ask").join("outside/secret.txt");
    let env = Env::new(
        "acp-live-ask",
        json!({ "steps": [
        { "call": "read", "args": { "path": outside.display().to_string() } }, { "text": "done" }
    ] }),
    );
    std::fs::create_dir_all(outside.parent().unwrap()).unwrap();
    std::fs::write(&outside, "secret value\n").unwrap();
    let (mut c, sid) = start(&env);
    prompt(&mut c, &sid);
    let old = loop {
        let next = c.read();
        if next["method"] == "session/request_permission" {
            break next;
        }
    };
    c.call("initialize", json!({ "protocolVersion": 1 }));
    // This retired answer arrives before load, so rekeying only at load fails.
    c.send(json!({ "jsonrpc": "2.0", "id": old["id"], "result": { "outcome": { "outcome": "selected", "optionId": "deny" } } }));
    c.answers.push_back("once");
    let (attached, replay) = load(&mut c, &env, &sid);
    assert_eq!(attached["result"]["_meta"]["construct"]["turn"]["state"], "waiting");
    let asks: Vec<_> = replay.iter().filter(|v| v["method"] == "session/request_permission").collect();
    assert_eq!(asks.len(), 1, "{replay:?}");
    assert_ne!(asks[0]["id"], old["id"]);
    assert_eq!(asks[0]["params"], old["params"], "only the RPC id changes");
    let calls = updates(&replay, "tool_call");
    assert_eq!(calls.len(), 1, "{replay:?}");
    assert_eq!(calls[0]["status"], "in_progress");
    let later = idle(&mut c, &sid);
    assert_eq!(text(&later), "done");
    assert_eq!(updates(&later, "tool_call_update")[0]["status"], "completed");
    assert!(later.iter().any(|v| v["method"] == "_construct/turn" && v["params"]["state"] == "working"));
    let rows = env.journal();
    let ends: Vec<_> = rows.iter().filter(|r| r.ty == "tool_end").collect();
    assert_eq!(ends.len(), 1);
    assert_eq!(ends[0].payload["ok"], true);
    assert!(ends[0].payload["output"].as_str().unwrap().contains("secret value"));
    assert!(rows.iter().all(|r| r.ty != "recovered" && r.payload["code"] != "interrupted_unknown"));
    c.finish();
}

#[test]
fn turn_finishing_detached_replays_whole_turn_and_idle_stop_reason() {
    let env = Env::new("acp-live-idle", json!({ "steps": [{ "text": "finished while detached", "delay_ms": 50 }] }));
    let (mut c, sid) = start(&env);
    prompt(&mut c, &sid);
    c.call("initialize", json!({ "protocolVersion": 1 }));
    let detached = idle(&mut c, &sid);
    assert!(updates(&detached, "agent_message_chunk").is_empty(), "detached stream is gated");
    let (attached, replay) = load(&mut c, &env, &sid);
    assert_eq!(text(&replay), "finished while detached");
    assert_eq!(
        attached["result"]["_meta"]["construct"]["turn"],
        json!({
            "sessionId": sid, "turn": 1, "state": "idle", "stopReason": "end_turn"
        })
    );
    assert!(c.pending.is_empty(), "no old prompt response");
    assert_eq!(env.journal().iter().filter(|r| r.ty == "assistant").count(), 1);
    c.finish();
}

#[test]
fn live_load_of_running_tool_never_recovers_its_outcome() {
    let env = Env::new(
        "acp-live-tool",
        json!({ "steps": [
        { "call": "shell", "args": { "command": "sleep 30" } }, { "text": "late" }
    ] }),
    );
    let (mut c, sid) = start(&env);
    prompt(&mut c, &sid);
    loop {
        if c.read()["params"]["update"]["sessionUpdate"] == "tool_call" {
            break;
        }
    }
    c.call("initialize", json!({ "protocolVersion": 1 }));
    let (attached, replay) = load(&mut c, &env, &sid);
    assert_eq!(attached["result"]["_meta"]["construct"]["turn"]["state"], "working");
    assert_eq!(updates(&replay, "tool_call")[0]["status"], "in_progress");
    c.notify("session/cancel", json!({ "sessionId": sid }));
    let later = idle(&mut c, &sid);
    assert_eq!(later.last().unwrap()["params"]["stopReason"], "cancelled");
    let rows = env.journal();
    assert_eq!(rows.iter().filter(|r| r.ty == "tool_start").count(), 1);
    assert!(rows.iter().all(|r| r.ty != "recovered" && r.payload["code"] != "interrupted_unknown"));
    c.finish();
}

#[test]
fn an_answer_before_initialize_is_not_asked_again() {
    let outside = Env::dir_for("acp-live-answered").join("outside/secret.txt");
    let env = Env::new(
        "acp-live-answered",
        json!({ "steps": [
        { "call": "read", "args": { "path": outside.display().to_string() } },
        { "text": "done", "delay_ms": 100 }
    ] }),
    );
    std::fs::create_dir_all(outside.parent().unwrap()).unwrap();
    std::fs::write(&outside, "read once\n").unwrap();
    let (mut c, sid) = start(&env);
    prompt(&mut c, &sid);
    let ask = loop {
        let next = c.read();
        if next["method"] == "session/request_permission" {
            break next;
        }
    };
    c.send(json!({ "jsonrpc": "2.0", "id": ask["id"], "result": {
        "outcome": { "outcome": "selected", "optionId": "once" }
    } }));
    c.call("initialize", json!({ "protocolVersion": 1 }));
    let (attached, replay) = load(&mut c, &env, &sid);
    assert!(replay.iter().all(|v| v["method"] != "session/request_permission"), "{replay:?}");
    assert_ne!(attached["result"]["_meta"]["construct"]["turn"]["state"], "waiting");
    idle(&mut c, &sid);
    assert!(env.journal().iter().any(|r| r.ty == "tool_end" && r.payload["ok"] == true));
    c.finish();
}

#[test]
fn cancelling_a_reissued_question_finishes_and_does_not_leave_a_waiter() {
    let outside = Env::dir_for("acp-live-cancel-ask").join("outside/secret.txt");
    let env = Env::new(
        "acp-live-cancel-ask",
        json!({ "steps": [
        { "call": "read", "args": { "path": outside.display().to_string() } }, { "text": "next turn" }
    ] }),
    );
    std::fs::create_dir_all(outside.parent().unwrap()).unwrap();
    std::fs::write(&outside, "must not be read\n").unwrap();
    let (mut c, sid) = start(&env);
    prompt(&mut c, &sid);
    loop {
        if c.read()["method"] == "session/request_permission" {
            break;
        }
    }
    c.call("initialize", json!({ "protocolVersion": 1 }));
    // Load without Client::call's automatic permission answer.
    let loading = c.request("session/load", json!({ "sessionId": sid, "cwd": env.work.display().to_string() }));
    let mut reissued = None;
    loop {
        let next = c.read();
        if next["method"] == "session/request_permission" {
            reissued = Some(next);
        } else if next["id"] == loading && next["method"].is_null() {
            break;
        }
    }
    assert!(reissued.is_some());
    c.notify("session/cancel", json!({ "sessionId": sid }));
    let later = idle(&mut c, &sid);
    assert_eq!(later.last().unwrap()["params"]["stopReason"], "cancelled");
    c.call("initialize", json!({ "protocolVersion": 1 }));
    let (attached, replay) = load(&mut c, &env, &sid);
    assert_eq!(attached["result"]["_meta"]["construct"]["turn"]["state"], "idle");
    assert!(replay.iter().all(|v| v["method"] != "session/request_permission"), "{replay:?}");
    let (done, _) =
        c.call("session/prompt", json!({ "sessionId": sid, "prompt": [{ "type": "text", "text": "again" }] }));
    assert_eq!(done["result"]["stopReason"], "end_turn");
    assert!(env.journal().iter().all(|r| r.ty != "tool_end" || r.payload["ok"] != true));
    c.finish();
}

#[test]
fn eof_during_a_permission_wait_exits_instead_of_stranding_the_turn() {
    let outside = Env::dir_for("acp-live-eof").join("outside/secret.txt");
    let env = Env::new(
        "acp-live-eof",
        json!({ "steps": [
        { "call": "read", "args": { "path": outside.display().to_string() } }
    ] }),
    );
    std::fs::create_dir_all(outside.parent().unwrap()).unwrap();
    std::fs::write(&outside, "hidden\n").unwrap();
    let (mut c, sid) = start(&env);
    prompt(&mut c, &sid);
    loop {
        if c.read()["method"] == "session/request_permission" {
            break;
        }
    }
    c.finish();
    assert!(env.journal().iter().any(|r| r.ty == "turn_end" && r.payload["interrupted"] == true));
}

#[test]
fn queued_prompts_keep_separate_turns_and_replay_in_order() {
    let env = Env::new(
        "acp-live-queue",
        json!({ "chunk_ms": 20, "steps": [
        { "text": "first answer" }, { "text": "second answer" }
    ] }),
    );
    let (mut c, sid) = start(&env);
    let first = prompt(&mut c, &sid);
    let second = prompt(&mut c, &sid);
    let (one, _) = c.await_response(first);
    let (two, _) = c.await_response(second);
    assert_eq!(one["result"]["stopReason"], "end_turn");
    assert_eq!(two["result"]["stopReason"], "end_turn");
    c.call("initialize", json!({ "protocolVersion": 1 }));
    let (attached, replay) = load(&mut c, &env, &sid);
    assert_eq!(attached["result"]["_meta"]["construct"]["turn"]["turn"], 2);
    assert_eq!(text(&replay), "first answersecond answer");
    assert_eq!(updates(&replay, "user_message_chunk").len(), 2);
    assert_eq!(env.requests().len(), 2);
    c.finish();
}

#[test]
fn every_pending_question_is_reissued_and_waiting_lasts_until_the_last_answer() {
    let outside = Env::dir_for("acp-live-many").join("outside");
    let env = Env::new(
        "acp-live-many",
        json!({ "steps": [ { "calls": [
        { "call": "read", "args": { "path": outside.join("one.txt").display().to_string() } },
        { "call": "read", "args": { "path": outside.join("two.txt").display().to_string() } }
    ] }, { "text": "both read" } ] }),
    );
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("one.txt"), "one\n").unwrap();
    std::fs::write(outside.join("two.txt"), "two\n").unwrap();
    let (mut c, sid) = start(&env);
    prompt(&mut c, &sid);
    let mut old = vec![];
    while old.len() < 2 {
        let next = c.read();
        if next["method"] == "session/request_permission" {
            old.push(next["id"].clone());
        }
    }
    c.call("initialize", json!({ "protocolVersion": 1 }));
    let loading = c.request("session/load", json!({ "sessionId": sid, "cwd": env.work.display().to_string() }));
    let mut asks = vec![];
    let mut calls = vec![];
    loop {
        let next = c.read();
        if next["method"] == "session/request_permission" {
            asks.push(next.clone());
        }
        if next["params"]["update"]["sessionUpdate"] == "tool_call" {
            calls.push(next.clone());
        }
        if next["id"] == loading && next["method"].is_null() {
            assert_eq!(next["result"]["_meta"]["construct"]["turn"]["state"], "waiting");
            break;
        }
    }
    assert_eq!(asks.len(), 2);
    assert_eq!(calls.len(), 2);
    assert!(calls.iter().all(|c| c["params"]["update"]["status"] == "in_progress"));
    assert!(asks.iter().all(|a| !old.contains(&a["id"])));
    let answer = |id: &Value| {
        json!({ "jsonrpc": "2.0", "id": id, "result": {
        "outcome": { "outcome": "selected", "optionId": "once" }
    } })
    };
    c.send(answer(&asks[0]["id"]));
    let (_, before_last) = c.call("unknown/barrier", json!({}));
    assert!(before_last.iter().all(|v| v["method"] != "_construct/turn" || v["params"]["state"] == "waiting"));
    c.send(answer(&asks[1]["id"]));
    let later = idle(&mut c, &sid);
    assert_eq!(text(&later), "both read");
    assert_eq!(env.journal().iter().filter(|r| r.ty == "tool_end" && r.payload["ok"] == true).count(), 2);
    c.finish();
}

#[test]
fn cancelled_partial_text_survives_the_next_turn_and_a_later_attachment() {
    let env = Env::new(
        "acp-live-partial-cancel",
        json!({ "chunk_ms": 30, "steps": [
        { "text": "one two three four five six seven eight nine ten eleven twelve thirteen fourteen fifteen" },
        { "text": "next answer" }
    ] }),
    );
    let (mut c, sid) = start(&env);
    let first = prompt(&mut c, &sid);
    let mut seen = vec![];
    loop {
        let next = c.read();
        let has_text = next["params"]["update"]["sessionUpdate"] == "agent_message_chunk";
        seen.push(next);
        if has_text {
            break;
        }
    }
    c.notify("session/cancel", json!({ "sessionId": sid }));
    let (cancelled, rest) = c.await_response(first);
    assert_eq!(cancelled["result"]["stopReason"], "cancelled");
    seen.extend(rest);
    let partial = text(&seen);
    assert!(!partial.is_empty());
    let (done, _) =
        c.call("session/prompt", json!({ "sessionId": sid, "prompt": [{ "type": "text", "text": "again" }] }));
    assert_eq!(done["result"]["stopReason"], "end_turn");
    c.call("initialize", json!({ "protocolVersion": 1 }));
    let (_, replay) = load(&mut c, &env, &sid);
    assert_eq!(text(&replay), partial + "next answer");
    assert_eq!(
        env.journal().iter().filter(|r| r.ty == "assistant").count(),
        1,
        "cancelled partial was never committed"
    );
    c.finish();
}
