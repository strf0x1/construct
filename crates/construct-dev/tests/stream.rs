//! What the client sends back and how long it waits (#47): a thinking
//! model's reasoning returns on the next request under the key it streamed
//! in, also after a resume, and a provider that stalls fails the turn instead
//! of hanging it.

use crate::common::*;
use serde_json::{Value, json};
use std::time::{Duration, Instant};

/// The assistant messages of a request body.
fn assistants(req: &Value) -> Vec<Value> {
    req["messages"].as_array().unwrap().iter().filter(|m| m["role"] == "assistant").cloned().collect()
}

fn thinking_call() -> Value {
    json!({ "call": "read", "args": { "path": "notes.txt" }, "reasoning": "the notes say where it is" })
}

#[test]
fn reasoning_goes_back_with_its_tool_call_and_survives_a_resume() {
    let env = Env::new(
        "stream-reasoning",
        json!({ "steps": [thinking_call(), { "text": "found it" }, { "text": "again" }] }),
    );
    std::fs::write(env.work.join("notes.txt"), "here\n").unwrap();
    let out = env.run(&["run", "--no-learn", "where is it?"]);
    assert!(out.status.success(), "{}", text(&out));

    let reqs = env.requests();
    assert_eq!(reqs.len(), 2);
    let sent = assistants(&reqs[1]);
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(sent[0]["reasoning_content"], "the notes say where it is");
    assert!(sent[0].get("reasoning").is_none(), "only the key the server streamed: {sent:?}");
    assert_eq!(sent[0]["tool_calls"][0]["function"]["name"].as_str().map(str::is_empty), Some(false));

    // The journal keeps the key, so a resumed session sends it too.
    let id = env.sessions()[0].clone();
    let out = env.run(&["--resume", &id, "run", "--no-learn", "and now?"]);
    assert!(out.status.success(), "{}", text(&out));
    let reqs = env.requests();
    let resumed = assistants(reqs.last().unwrap());
    assert_eq!(resumed[0]["reasoning_content"], "the notes say where it is", "{resumed:?}");
}

#[test]
fn reasoning_drop_keeps_it_off_the_wire() {
    let env = Env::new("stream-reasoning-drop", json!({ "steps": [thinking_call(), { "text": "found it" }] }));
    env.config("[provider]\nreasoning = \"drop\"\n");
    std::fs::write(env.work.join("notes.txt"), "here\n").unwrap();
    let out = env.run(&["run", "--no-learn", "where is it?"]);
    assert!(out.status.success(), "{}", text(&out));
    let sent = assistants(&env.requests()[1]);
    assert!(sent[0].get("reasoning_content").is_none(), "{sent:?}");
    // Still journaled for the transcript.
    let row = env.journal().into_iter().find(|r| r.ty == "assistant").unwrap();
    assert_eq!(row.payload["message"]["reasoning"], "the notes say where it is");
}

#[test]
fn a_stream_that_stalls_mid_answer_fails_instead_of_hanging() {
    let env = Env::new("stream-stall", json!({ "steps": [{ "text": "half of it", "stall_ms": 30000 }] }));
    env.config("[provider]\nidle_timeout = 1\n");
    let start = Instant::now();
    let out = env.run(&["run", "--no-learn", "hi"]);
    let took = start.elapsed();
    assert!(!out.status.success(), "{}", text(&out));
    assert!(text(&out).contains("stalled") && text(&out).contains("idle_timeout"), "{}", text(&out));
    assert!(took < Duration::from_millis(budget(10_000)), "took {took:?}");
}

#[test]
fn a_silent_endpoint_fails_at_the_first_byte_deadline() {
    let env = Env::new("stream-silent", json!({ "steps": [{ "text": "late", "delay_ms": 30000 }] }));
    env.config("[provider]\nfirst_byte_timeout = 1\n");
    let start = Instant::now();
    let out = env.run(&["run", "--no-learn", "hi"]);
    let took = start.elapsed();
    assert!(!out.status.success(), "{}", text(&out));
    assert!(text(&out).contains("first_byte_timeout"), "{}", text(&out));
    assert!(took < Duration::from_millis(budget(10_000)), "took {took:?}");
}
