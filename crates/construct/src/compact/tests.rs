//! What the planner cuts, elides and summarizes: the tests for `plan`, the
//! stub glosses and the summary budget.
use super::*;
use crate::llm::{FunctionCall, ToolCall};

fn call(id: &str) -> Message {
    Message {
        role: "assistant".into(),
        content: None,
        tool_calls: vec![ToolCall {
            id: id.into(),
            kind: "function".into(),
            function: FunctionCall { name: "read".into(), arguments: "{}".into() },
        }],
        tool_call_id: None,
        reasoning: None,
    }
}

/// One user request followed by `n` rounds of a call and a `size`-char result.
fn single_turn(n: usize, size: usize) -> Vec<Message> {
    let mut m = vec![Message::system("sys"), Message::user("the task")];
    for i in 0..n {
        m.push(call(&format!("c{i}")));
        m.push(Message::tool(&format!("c{i}"), format!("line\n{}", "x".repeat(size))));
    }
    m
}

fn chars(m: &[Message]) -> u64 {
    m.iter().map(|m| m.content.as_deref().map_or(0, str::len) as u64 + 20).sum()
}

/// Every tool result follows the assistant message that called it.
fn paired(m: &[Message]) -> bool {
    let mut open: BTreeSet<String> = BTreeSet::new();
    for x in m {
        match x.role.as_str() {
            "assistant" => open = x.tool_calls.iter().map(|c| c.id.clone()).collect(),
            "tool" if !open.remove(x.tool_call_id.as_deref().unwrap_or("")) => return false,
            _ => {}
        }
    }
    true
}

#[test]
fn a_single_long_turn_is_cut_by_eliding() {
    let msgs = single_turn(20, 1000);
    let budget = chars(&msgs) * 10 / 9;
    let p = plan(&msgs, &chars, budget, &CompactionConfig::default(), Reason::Threshold).expect("a cut");
    assert_eq!(p.mode, Mode::Elide);
    assert_eq!(p.elide.len(), 18, "all but the last two rounds");
    assert!(p.reached);
    let after = apply(msgs, &p.cut(None));
    assert!(paired(&after));
    assert!(after[3].content.as_deref().unwrap().starts_with(ELIDED));
    assert!(!after.last().unwrap().content.as_deref().unwrap().starts_with(ELIDED));
}

#[test]
fn eliding_escalates_to_all_but_the_newest_round_before_summarizing() {
    // Two huge recent rounds: stubbing outside them misses the target.
    let mut msgs = single_turn(6, 1000);
    for i in 6..8 {
        msgs.push(call(&format!("c{i}")));
        msgs.push(Message::tool(&format!("c{i}"), "z".repeat(4000)));
    }
    let budget = chars(&msgs) * 10 / 9;
    let p = plan(&msgs, &chars, budget, &CompactionConfig::default(), Reason::Threshold).expect("a cut");
    assert_eq!((p.mode, p.elide.len(), p.reached), (Mode::Elide, 7, true));
    assert!(!p.elide.contains(&"c7".to_string()), "the newest round is kept");
}

#[test]
fn under_the_threshold_nothing_happens_unless_aging() {
    let msgs = single_turn(20, 1000);
    let budget = chars(&msgs) * 3;
    assert_eq!(plan(&msgs, &chars, budget, &CompactionConfig::default(), Reason::Threshold), None);
    let aging = CompactionConfig { age_rounds: 16, ..Default::default() };
    let p = plan(&msgs, &chars, budget, &aging, Reason::Threshold).expect("an age cut");
    assert_eq!((p.mode, p.reason, p.elide.len()), (Mode::Elide, Reason::Age, 4));
}

#[test]
fn big_calls_need_a_summary_that_pins_the_request() {
    // Results are tiny, the calls' own text is what fills the window.
    let mut msgs = vec![Message::system("sys"), Message::user("the task")];
    for i in 0..12 {
        let mut c = call(&format!("c{i}"));
        c.content = Some("y".repeat(3000));
        msgs.push(c);
        msgs.push(Message::tool(&format!("c{i}"), "ok"));
    }
    let budget = chars(&msgs) * 10 / 9;
    let p = plan(&msgs, &chars, budget, &CompactionConfig::default(), Reason::Threshold).expect("a cut");
    assert_eq!(p.mode, Mode::Summarize);
    assert_eq!(p.request.as_deref(), Some("the task"));
    let after = apply(msgs, &p.cut(Some("SUMMARY".into())));
    assert!(paired(&after));
    let head = after[1].content.as_deref().unwrap();
    assert!(head.contains("SUMMARY") && head.ends_with("the task"));
    assert_eq!(after[2].role, "assistant", "the tail starts at a round");

    // A second summary carries the pinned request forward.
    let mut more = after.clone();
    for i in 12..24 {
        let mut c = call(&format!("c{i}"));
        c.content = Some("y".repeat(3000));
        more.push(c);
        more.push(Message::tool(&format!("c{i}"), "ok"));
    }
    let p2 = plan(&more, &chars, budget, &CompactionConfig::default(), Reason::Threshold).expect("a second cut");
    assert_eq!(p2.request.as_deref(), Some("the task"));
    let req = summary_request(&more[1..p2.upto.unwrap()], 1_000_000);
    assert!(req.starts_with("EARLIER SUMMARY:\nSUMMARY"));
}

#[test]
fn overflow_forces_a_cut_below_the_threshold() {
    let msgs = single_turn(10, 1000);
    let budget = chars(&msgs) * 4;
    let p = plan(&msgs, &chars, budget, &CompactionConfig::default(), Reason::Overflow).expect("forced");
    assert_eq!(p.reason, Reason::Overflow);
    assert!(!p.elide.is_empty());
}

#[test]
fn nothing_left_to_cut_is_none() {
    let msgs = vec![Message::system("sys"), Message::user("x".repeat(100_000))];
    assert_eq!(plan(&msgs, &chars, 1000, &CompactionConfig::default(), Reason::Overflow), None);
}

#[test]
fn overflow_errors_are_recognised() {
    let e = r#"model endpoint returned 400 Bad Request: {"error":{"message":"This model's maximum context length is 32768 tokens. However, your messages resulted in 40123 tokens.","code":"context_length_exceeded"}}"#;
    assert!(is_overflow(e));
    assert_eq!(overflow_tokens(e), Some(40123));
    assert_eq!(overflow_limit(e), Some(32768));
    assert!(!is_overflow("model endpoint returned 401 Unauthorized"));
}
