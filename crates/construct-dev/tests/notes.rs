//! What the provider client used to swallow, as the journal now records it
//! (#48): retries, garbled chunks, a cut stream and a rejected parameter each
//! become one `note` row per model call, and a clean call leaves none.

use crate::common::*;
use serde_json::{Value, json};

fn notes(env: &Env) -> Vec<Value> {
    env.journal().into_iter().filter(|r| r.ty == "note").map(|r| r.payload).collect()
}

/// A note's `kind` and `count`, in journal order.
fn kinds(notes: &[Value]) -> Vec<(String, u64)> {
    notes.iter().map(|n| (n["kind"].as_str().unwrap().to_string(), n["count"].as_u64().unwrap())).collect()
}

#[test]
fn a_flaky_call_is_one_row_per_kind_and_the_turn_still_finishes() {
    let env = Env::new(
        "notes-flaky",
        json!({ "steps": [
            { "status": 503, "retry_after": 0 },
            { "status": 503, "retry_after": 0 },
            { "text": "all good now", "bad_chunks": 3 },
        ] }),
    );
    let out = env.run(&["run", "--json", "--no-learn", "hi"]);
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(env.requests().len(), 3, "two retries, then the answer");

    let notes = notes(&env);
    let mut found = kinds(&notes);
    found.sort();
    assert_eq!(found, [("bad_chunk".into(), 3), ("retry".into(), 2)], "{notes:?}");
    assert!(notes.iter().all(|n| n["class"] == "provider"));
    let retry = notes.iter().find(|n| n["kind"] == "retry").unwrap();
    assert!(retry["detail"].as_str().unwrap().contains("503"), "{notes:?}");

    // The notes land before the call's usage, and the answer arrived intact.
    let rows = env.journal();
    let seq = |ty: &str| rows.iter().find(|r| r.ty == ty).unwrap_or_else(|| panic!("no {ty} row")).seq;
    assert!(seq("request") < seq("note") && seq("note") < seq("usage"));
    let answer = rows.iter().rev().find(|r| r.ty == "assistant").unwrap();
    assert_eq!(answer.payload["message"]["content"], "all good now");

    // `--json` carries them too, and the transcript shows them.
    let printed = String::from_utf8_lossy(&out.stdout);
    assert!(printed.lines().filter(|l| l.contains(r#""event":"note""#)).count() == 2, "{printed}");
    let shown = text(&env.run(&["sessions", "show", &env.sessions()[0]]));
    assert!(shown.contains("note provider/retry x2") && shown.contains("note provider/bad_chunk x3"), "{shown}");
}

#[test]
fn a_clean_call_writes_no_note() {
    let env = Env::new("notes-clean", json!({ "steps": [{ "text": "fine" }] }));
    let out = env.run(&["run", "--no-learn", "hi"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(notes(&env).is_empty());
}

#[test]
fn a_stream_that_ends_without_a_finish_is_noted_not_failed() {
    let env = Env::new("notes-cut", json!({ "steps": [{ "text": "half an answ", "cut": true }] }));
    let out = env.run(&["run", "--no-learn", "hi"]);
    assert!(out.status.success(), "the partial answer is kept: {}", text(&out));
    let notes = notes(&env);
    assert_eq!(kinds(&notes), [("truncated".into(), 1)], "{notes:?}");
    assert!(notes[0]["detail"].as_str().unwrap().contains("[DONE]"));
}

#[test]
fn a_rejected_parameter_is_noted_beside_the_error() {
    let rejected = r#"{"error":{"message":"Unsupported parameter: 'stream_options' is not allowed"}}"#;
    let env = Env::new("notes-param", json!({ "steps": [{ "status": 400, "body": rejected }] }));
    let out = env.run(&["run", "--no-learn", "hi"]);
    assert!(!out.status.success(), "a 400 is still fatal: {}", text(&out));
    assert_eq!(env.requests().len(), 1, "a 400 is not retried");

    let notes = notes(&env);
    assert_eq!(kinds(&notes), [("param_rejected".into(), 1)], "{notes:?}");
    assert!(notes[0]["detail"].as_str().unwrap().starts_with("stream_options:"), "{notes:?}");
    // The note is not a second error: the user-visible signal stays `error`.
    let errors = env.journal().into_iter().filter(|r| r.ty == "error").count();
    assert_eq!(errors, 1);
}
