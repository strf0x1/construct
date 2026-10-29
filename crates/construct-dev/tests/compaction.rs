//! Compaction end to end: a scripted model with a small context window, and
//! what the model is sent as history grows. Every request must fit the
//! window, keep each tool result after the call that asked for it, and keep
//! the user's request; resume must see the compacted view, not the original.

mod common;

use common::*;
use serde_json::{Value, json};

/// `n` rounds that each read a ~8 KB file, then an answer.
fn reads(n: usize) -> Value {
    let mut steps: Vec<Value> =
        (0..n).map(|i| json!({ "call": "read", "args": { "path": format!("big{}.txt", i % 3) } })).collect();
    steps.push(json!({ "text": "done reading" }));
    json!({ "steps": steps })
}

fn big_files(env: &Env) {
    for i in 0..3 {
        let body: String = (0..200).map(|l| format!("file {i} line {l:03}: some ordinary content here\n")).collect();
        std::fs::write(env.work.join(format!("big{i}.txt")), body).unwrap();
    }
}

/// Requests from the agent loop (the summarizer's carry no tools).
fn agent_requests(env: &Env) -> Vec<Value> {
    env.requests().into_iter().filter(|r| r["tools"].as_array().is_some_and(|t| !t.is_empty())).collect()
}

fn paired(req: &Value) -> bool {
    let mut open: Vec<String> = vec![];
    for m in req["messages"].as_array().unwrap() {
        match m["role"].as_str().unwrap() {
            "assistant" => {
                open = m["tool_calls"]
                    .as_array()
                    .map(|a| a.iter().map(|c| c["id"].as_str().unwrap().to_string()).collect())
                    .unwrap_or_default()
            }
            "tool" => {
                let id = m["tool_call_id"].as_str().unwrap();
                let Some(i) = open.iter().position(|x| x == id) else { return false };
                open.remove(i);
            }
            _ => {}
        }
    }
    true
}

fn log(env: &Env) -> Vec<Value> {
    std::fs::read_to_string(env.dir.join("log.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn compactions(env: &Env) -> Vec<Value> {
    env.journal().into_iter().filter(|r| r.ty == "compacted").map(|r| r.payload).collect()
}

#[test]
fn a_long_single_turn_is_cut_by_eliding_and_resume_sees_the_cut() {
    let env = Env::with_window("compact-elide", reads(12), Some(16_384));
    env.config("context_window = 16384\n");
    big_files(&env);
    let out = env.run(&["run", "--no-learn", "read the big files"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(text(&out).contains("done reading"));

    assert!(log(&env).iter().all(|r| r["window_rejected"].is_null()), "no request overflowed");
    let cuts = compactions(&env);
    assert!(!cuts.is_empty(), "a single long turn compacts");
    assert!(
        cuts.iter().all(|c| c["mode"] == "elide" && c["reason"] == "threshold" && c["reached"] == true),
        "{cuts:?}"
    );
    let reqs = agent_requests(&env);
    assert!(reqs.iter().all(paired), "calls stay paired with results");
    let last = reqs.last().unwrap().to_string();
    assert!(last.contains("[elided to save context:"));
    assert!(last.contains("read the big files"), "the request is never cut");

    // Resume replays the cut: the next session starts from the compacted view.
    let before = reqs.last().unwrap()["messages"].as_array().unwrap().len();
    let out = env.run(&["run", "--no-learn", "-c", "and then?"]);
    assert!(out.status.success(), "{}", text(&out));
    let resumed = agent_requests(&env).last().unwrap().clone();
    assert!(resumed.to_string().contains("[elided to save context:"), "resume sees stubs, not the originals");
    assert!(resumed["messages"].as_array().unwrap().len() >= before);
    assert!(log(&env).iter().all(|r| r["window_rejected"].is_null()));
}

#[test]
fn overflow_is_recovered_by_a_forced_cut_and_one_resend() {
    // construct believes the window is 128k; the model's is 12k.
    let env = Env::with_window("compact-overflow", reads(10), Some(12_000));
    big_files(&env);
    let out = env.run(&["run", "--no-learn", "read the big files"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(text(&out).contains("done reading"), "{}", text(&out));
    let rejected: Vec<usize> =
        log(&env).iter().enumerate().filter(|(_, r)| r["window_rejected"] == true).map(|(i, _)| i).collect();
    assert_eq!(rejected.len(), 1, "after one rejection construct plans against the window the provider stated");
    let l = log(&env);
    for i in rejected {
        assert!(l.get(i + 1).is_some_and(|r| r["window_rejected"].is_null()), "every rejection's resend fits");
    }
    let cuts = compactions(&env);
    assert!(cuts.iter().any(|c| c["reason"] == "overflow"), "{cuts:?}");
    assert!(agent_requests(&env).iter().all(paired));
}

#[test]
fn rounds_too_big_to_elide_are_summarized_with_the_request_pinned() {
    // The calls themselves (write contents) fill the window; results are tiny.
    let mut steps: Vec<Value> = (0..10)
        .map(|i| json!({ "call": "write", "args": { "path": format!("out{i}.txt"), "content": "w".repeat(6000) } }))
        .collect();
    steps.push(json!({ "text": "all written" }));
    let env = Env::with_window(
        "compact-summary",
        json!({ "steps": steps, "side": ["## Goal\nSUMMARY-1", "## Goal\nSUMMARY-2", "## Goal\nSUMMARY-3"] }),
        Some(16_384),
    );
    // A catalog price, so the summaries are priced as the loop's requests are.
    env.config(
        "context_window = 16384\nmodel = \"test/priced\"\n[models.\"test/priced\"]\npricing = { prompt = 1.0, completion = 2.0 }\n",
    );
    let out = env.run(&["run", "--no-learn", "write ten files of w"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(text(&out).contains("all written"));
    assert!((0..10).all(|i| env.work.join(format!("out{i}.txt")).exists()));

    let cuts = compactions(&env);
    let s = cuts.iter().find(|c| c["mode"] == "summarize").unwrap_or_else(|| panic!("a summary cut: {cuts:?}"));
    assert!(s["summary_prompt"].as_u64().unwrap() > 0, "the summarizer's usage is journaled");
    assert!(s["summary_cost"].as_f64().unwrap() > 0.0, "and priced");
    // The session's totals (the status line, `/status`) bill the summaries too.
    let db = construct::journal::Journal::open(&env.journal_path(&env.work)).unwrap();
    let id = env.sessions().remove(0);
    let totals = construct::journal::Totals::session(&db, &db.since(&id, 0).unwrap()).unwrap();
    let summarized: Vec<&Value> = cuts.iter().filter(|c| c["mode"] == "summarize").collect();
    let usage_rows = env.journal().into_iter().filter(|r| r.ty == "usage").count() as u64;
    assert_eq!(totals.requests, usage_rows + summarized.len() as u64);
    let summary_prompt: u64 = summarized.iter().map(|c| c["summary_prompt"].as_u64().unwrap()).sum();
    assert_eq!(totals.side.prompt_tokens, summary_prompt);
    // The summarizer was asked with its own prompt and the replaced rounds;
    // a second summary is asked to build on the first.
    let side: Vec<Value> =
        env.requests().into_iter().filter(|r| r["tools"].as_array().is_none_or(|t| t.is_empty())).collect();
    assert!(side[0]["messages"][0]["content"].as_str().unwrap().starts_with("You compact an agent's working context"));
    assert!(side.len() >= 2, "ten big rounds in a 16k window take more than one summary");
    let second = side[1]["messages"][1]["content"].as_str().unwrap();
    assert!(second.starts_with("EARLIER SUMMARY:\n## Goal\nSUMMARY-1"), "{second}");
    let reqs = agent_requests(&env);
    assert!(reqs.iter().all(paired));
    let last = &reqs.last().unwrap()["messages"];
    let head = last[1]["content"].as_str().unwrap();
    assert!(head.contains(&format!("SUMMARY-{}", side.len())), "{head}");
    assert!(head.ends_with("write ten files of w"), "the request is pinned verbatim under the summary");
    assert!(log(&env).iter().all(|r| r["window_rejected"].is_null()));
}
