//! Compaction end to end: a scripted model with a small context window, and
//! what the model is sent as history grows. Every request must fit the
//! window, keep each tool result after the call that asked for it, and keep
//! the user's request; resume must see the compacted view, not the original.

use crate::common::*;
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
fn a_4k_window_is_planned_as_4k_and_reads_page_to_fit_it() {
    // #95: the window was floored at 8192, so a 4k model overflowed once
    // before construct learned its size; and one ~10 KB read was most of it.
    let env = Env::with_window("compact-4k", reads(6), Some(4096));
    env.config("context_window = 4096\n");
    big_files(&env);
    let out = env.run(&["run", "--no-learn", "read the big files"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(text(&out).contains("done reading"));
    assert!(log(&env).iter().all(|r| r["window_rejected"].is_null()), "no request overflowed, the first included");

    let reads: Vec<Value> = env.journal().into_iter().filter(|r| r.ty == "tool_end").map(|r| r.payload).collect();
    assert!(!reads.is_empty());
    for r in &reads {
        let out = r["output"].as_str().unwrap();
        assert!(out.len() < 4096, "a result fits a quarter of the budget: {} bytes", out.len());
        assert!(out.contains("Use offset to read more"), "a read cut short says how to go on: {out}");
    }
    let cuts = compactions(&env);
    assert!(!cuts.is_empty(), "the journal shows the cuts");
    assert!(cuts.iter().all(|c| c["budget"] == 3584), "planned against 4096 less its reserve: {cuts:?}");
}

#[test]
fn a_round_of_parallel_reads_shares_one_cap_and_fits_an_8k_window() {
    // #95 pilot: models read a whole package in one round, and the newest
    // round is never cut, so ten reads at one result's cap overflowed 8k.
    let reads: Vec<Value> =
        (0..10).map(|i| json!({ "call": "read", "args": { "path": format!("big{}.txt", i % 3) } })).collect();
    let env = Env::with_window(
        "compact-round",
        json!({ "steps": [{ "calls": reads }, { "text": "done reading" }] }),
        Some(8192),
    );
    env.config("context_window = 8192\n");
    big_files(&env);
    let out = env.run(&["run", "--no-learn", "read the big files"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(text(&out).contains("done reading"));
    assert!(log(&env).iter().all(|r| r["window_rejected"].is_null()), "the round fit the window");
    let outs: Vec<usize> = env
        .journal()
        .into_iter()
        .filter(|r| r.ty == "tool_end")
        .map(|r| r.payload["output"].as_str().unwrap().len())
        .collect();
    assert_eq!(outs.len(), 10);
    // Half of a 7168-token budget, at four bytes a token, split ten ways.
    assert!(outs.iter().all(|&n| n <= 1434), "each read paged to its share: {outs:?}");
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

/// Rounds of `writes_with_cut_off_summaries` that pass the threshold of a 16k
/// window but still fit it.
const N_FIT: usize = 6;

/// `n` rounds whose calls (write contents) are big and whose results are too
/// small to stub, then an answer; every summary hits the output limit.
fn writes_with_cut_off_summaries(n: usize) -> Value {
    let mut steps: Vec<Value> = (0..n)
        .map(|i| json!({ "call": "write", "args": { "path": format!("out{i}.txt"), "content": "w".repeat(6000) } }))
        .collect();
    steps.push(json!({ "text": "all written" }));
    let side: Vec<Value> = (0..10).map(|_| json!({ "text": "## Goal\nhalf a summ", "length": true })).collect();
    json!({ "steps": steps, "side": side })
}

#[test]
fn a_summary_cut_off_below_the_window_is_a_note_and_the_run_succeeds() {
    let env = Env::with_window("compact-summary-limit", writes_with_cut_off_summaries(N_FIT), Some(16_384));
    env.config("context_window = 16384\n");
    let out = env.run(&["run", "--no-learn", "write the files of w"]);
    assert!(out.status.success(), "a recovered compaction failure doesn't fail the run: {}", text(&out));
    assert!(text(&out).contains("all written"));
    assert!(log(&env).iter().all(|r| r["window_rejected"].is_null()), "the uncut prompt still fit");

    let rows = env.journal();
    assert!(rows.iter().all(|r| r.ty != "error"), "{:?}", rows.iter().map(|r| &r.ty).collect::<Vec<_>>());
    let notes: Vec<&Value> =
        rows.iter().filter(|r| r.ty == "note" && r.payload["class"] == "compaction").map(|r| &r.payload).collect();
    assert!(!notes.is_empty(), "the failed summary is journaled");
    assert!(notes.iter().all(|n| n["kind"] == "summary_failed"), "{notes:?}");
    assert!(notes[0]["detail"].as_str().unwrap().contains("hit the output limit"), "{notes:?}");
    assert!(rows.iter().any(|r| r.ty == "turn_end"));
}

#[test]
fn a_summary_cut_off_on_overflow_still_fails_the_run() {
    // construct believes the window is 128k; the model's is 12k, and with
    // nothing to stub and no summary there is nothing left to cut.
    let env = Env::with_window("compact-summary-limit-overflow", writes_with_cut_off_summaries(10), Some(12_000));
    let out = env.run(&["run", "--no-learn", "write the files of w"]);
    assert!(!out.status.success(), "{}", text(&out));
    let errors: Vec<String> =
        env.journal().into_iter().filter(|r| r.ty == "error").map(|r| r.payload["message"].to_string()).collect();
    assert!(errors.iter().any(|e| e.contains("compaction failed: the summary hit the output limit")), "{errors:?}");
}

#[test]
fn a_resumed_turn_is_sized_with_the_ratio_its_last_request_measured() {
    // #218: dense content (2 bytes a token) at a 64k window. Every user turn
    // is a new process, and the ratio learned in the last one was thrown away,
    // so a turn's first request was sized at the 4.0 default: it passed the
    // 0.7 threshold and then the window. The journal's last request and the
    // usage that answered it restore the measured ratio.
    let env = Env::with_window_bytes("compact-resume-ratio", json!({ "steps": [] }), Some(65_536), 2.0);
    env.config("context_window = 65536\n");
    for turn in 0..8 {
        let report = format!("report {turn}: {}", "x".repeat(20_000));
        let out = env.run(&["run", "--no-learn", "-c", &report]);
        assert!(out.status.success(), "turn {turn}: {}", text(&out));
    }
    let rejected: Vec<Value> = log(&env).into_iter().filter(|r| r["window_rejected"] == true).collect();
    assert!(rejected.is_empty(), "no request passed the window: {rejected:?}");
    let cuts = compactions(&env);
    assert!(cuts.iter().any(|c| c["reason"] == "threshold"), "the threshold cut before the window: {cuts:?}");
}
