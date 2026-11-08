//! The `optchat` strategy end to end (#204, ADR 0043): three turns through
//! the TUI, and what the model is sent on the third. The earlier turns must be
//! `<chat>` lines only; none of their raw messages may follow. The scripted
//! endpoint answers summary calls from `side` and the loop from `steps`, so
//! the two kinds of request are scripted apart.

use crate::common::*;
use construct_dev::drive;
use serde_json::{Value, json};

fn play(d: &mut drive::Driver, script: &str) {
    let mut log = vec![];
    if let Err(e) = drive::run_script(d, script, &mut log) {
        panic!("{e:#}\n{}", String::from_utf8_lossy(&log));
    }
}

/// Requests from the agent loop; a summary call carries no tools.
fn agent_requests(env: &Env) -> Vec<Value> {
    env.requests().into_iter().filter(|r| r["tools"].as_array().is_some_and(|t| !t.is_empty())).collect()
}

fn parts_text(message: &Value) -> String {
    message["content"]
        .as_array()
        .map(|parts| parts.iter().filter_map(|p| p["text"].as_str()).collect::<Vec<_>>().join(""))
        .unwrap_or_default()
}

#[test]
fn the_third_turn_sees_earlier_turns_as_chat_lines_and_never_raw() {
    let env = Env::with_window(
        "optchat-three-turns",
        json!({
            "steps": [{"text": "answer one"}, {"text": "answer two"}, {"text": "answer three"}],
            "side": ["merged one", "merged two", "merged three", "merged four"]
        }),
        Some(65_536),
    );
    env.config("context_window = 65536\n[learn]\nenabled = false\n");
    let mut vars = env.env();
    vars.push(("CONSTRUCT_CONTEXT_STRATEGY".into(), "optchat".into()));

    let mut d =
        drive::Driver::spawn(&[construct().display().to_string()], &vars, Some(env.work.to_str().unwrap()), 100, 30)
            .expect("the TUI starts");
    play(
        &mut d,
        "wait \"ask construct\" 8000\n\
         type first\nkey enter\nwait \"answer one\" 15000\nwait \"ask construct anything… (/help)\" 5000\n\
         type second\nkey enter\nwait \"answer two\" 15000\nwait \"ask construct anything… (/help)\" 5000\n\
         type third\nkey enter\nwait \"answer three\" 15000\n\
         wait \"ask construct anything… (/help)\" 5000\n",
    );

    let reqs = agent_requests(&env);
    assert_eq!(reqs.len(), 3, "one request per turn: {reqs:?}");
    let third = &reqs[2];
    let messages = third["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 2, "system and one user message, nothing raw from turns 1-2: {messages:?}");
    assert_eq!(messages[0]["role"], "system");
    assert_eq!(messages[1]["role"], "user");
    let chat = parts_text(&messages[1]);
    for line in ["user: first", "assistant: answer one", "user: second", "assistant: answer two"] {
        assert!(chat.contains(line), "the third request's <chat> lacks {line:?}: {chat:?}");
    }
    assert!(chat.contains("<chat>") && chat.contains("</chat>"));
    assert!(chat.trim_end().ends_with("third"), "the opening message is raw and last: {chat:?}");

    let memory = env.journal().into_iter().filter(|r| r.ty == "memory").count();
    assert!(memory > 0, "the journal records the fits");
}
