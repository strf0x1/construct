//! A crash after every journaled row, recovered by a fresh process (#244).
//! `Writer::emit` commits a row before it sends the event (ADR 0034), so a
//! journal cut after seq k is the state a crash right after commit k leaves.
//! One rich session is recorded once; each cut is resumed and must restore,
//! re-run no tool, send a paired request, and repair at most once.

use crate::common::*;
use serde_json::{Value, json};
use std::path::Path;

const WINDOW: &str = "context_window = 16384\n[router]\nmode = 'local'\n[learn]\nenabled = false\n";

/// Parallel calls with an outside effect, reads until the window compacts, a
/// background job whose completion wakes a turn, then a second turn's call.
fn script() -> Value {
    let mut steps = vec![json!({"calls":[
        {"call":"read","args":{"path":"big0.txt"}},
        {"call":"shell","args":{"command":"echo first >> effects.txt; printf first-done"}}
    ]})];
    steps.extend((0..10).map(|i| json!({"call":"read","args":{"path":format!("big{}.txt", i % 3)}})));
    steps.push(json!({"call":"shell","args":{"command":"sleep 0.2; printf bg-done","background":true}}));
    steps.push(json!({"text":"waiting for the job"}));
    steps.push(json!({"text":"saw the job"}));
    steps.push(json!({"call":"shell","args":{"command":"echo second >> effects.txt; printf second-done"}}));
    steps.push(json!({"text":"second turn done"}));
    json!({ "steps": steps })
}

fn big_files(env: &Env) {
    for i in 0..3 {
        let body: String = (0..200).map(|l| format!("file {i} line {l:03}: some ordinary content here\n")).collect();
        std::fs::write(env.work.join(format!("big{i}.txt")), body).unwrap();
    }
}

/// Every call's id has exactly one result, and nothing intervenes before it.
fn paired(req: &Value) -> Result<(), String> {
    let mut pending = std::collections::BTreeSet::new();
    for m in req["messages"].as_array().into_iter().flatten() {
        if m["role"] == "tool" {
            let id = m["tool_call_id"].as_str().unwrap_or_default();
            if !pending.remove(id) {
                return Err(format!("unpaired result {id}"));
            }
        } else {
            if !pending.is_empty() {
                return Err(format!("{} before results for {pending:?}", m["role"]));
            }
            for c in m["tool_calls"].as_array().into_iter().flatten() {
                pending.insert(c["id"].as_str().unwrap_or_default().to_string());
            }
        }
    }
    if pending.is_empty() { Ok(()) } else { Err(format!("no results for {pending:?}")) }
}

fn agent_requests(env: &Env) -> Vec<Value> {
    env.requests().into_iter().filter(|r| r["tools"].as_array().is_some_and(|t| !t.is_empty())).collect()
}

/// Put `pristine` back as the journal, cut after `seq`: a crash right after that commit.
fn crash_at(pristine: &Path, journal: &Path, seq: i64) {
    for ext in ["-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{ext}", journal.display()));
    }
    std::fs::copy(pristine, journal).unwrap();
    let db = rusqlite::Connection::open(journal).unwrap();
    db.execute("DELETE FROM events WHERE seq > ?1", [seq]).unwrap();
}

/// Resume once and check what a crash at `seq` must leave; prose for each breach.
fn resume(env: &Env, id: &str, seq: i64) -> Vec<String> {
    let effects = std::fs::read_to_string(env.work.join("effects.txt")).unwrap_or_default();
    let sent = agent_requests(env).len();
    let out = env.run(&["--resume", id, "run", "--no-learn", "continue"]);
    let mut breaches = vec![];
    if !out.status.success() {
        breaches.push(format!("resume failed: {}", text(&out).trim()));
    }
    let after: Vec<_> = env.journal().into_iter().filter(|r| r.seq > seq).collect();
    let retried = after.iter().filter(|r| r.ty == "tool_start" || r.ty == "job_start").count();
    if retried > 0 {
        breaches.push(format!("{retried} tool or job started again"));
    }
    if std::fs::read_to_string(env.work.join("effects.txt")).unwrap_or_default() != effects {
        breaches.push("an outside effect happened again".into());
    }
    let repairs = after.iter().filter(|r| r.ty == "recovered").count();
    if repairs > 1 {
        breaches.push(format!("{repairs} recovered rows"));
    }
    let reqs = agent_requests(env);
    if reqs.len() == sent {
        breaches.push("no request was sent".into());
    }
    for r in reqs.iter().skip(sent) {
        if let Err(e) = paired(r) {
            breaches.push(format!("request not paired: {e}"));
        }
    }
    breaches
}

#[test]
fn a_crash_after_any_journaled_row_recovers_without_retrying_anything() {
    let env = Env::with_window("crash-sweep", script(), Some(16_384));
    env.config(WINDOW);
    big_files(&env);
    let out = env.run(&["run", "--no-learn", "read the files, then wait for a job"]);
    assert!(out.status.success(), "{}", text(&out));
    let id = env.sessions()[0].clone();
    let out = env.run(&["--resume", &id, "run", "--no-learn", "one more change"]);
    assert!(out.status.success(), "{}", text(&out));

    // The recording must hold every boundary this sweep is meant to cross.
    let rows = env.journal();
    for ty in ["compacted", "job_start", "job_end", "turn_end", "tool_end"] {
        assert!(rows.iter().any(|r| r.ty == ty), "the recorded session has no {ty} row");
    }
    assert!(rows.iter().all(|r| r.session == id), "one session only");

    let journal = env.journal_path(&env.work);
    let pristine = env.dir.join("pristine.sqlite");
    let db = rusqlite::Connection::open(&journal).unwrap();
    db.execute("VACUUM INTO ?1", [pristine.to_str().unwrap()]).unwrap();
    drop(db);

    let mut failures = vec![];
    let mut repaired_cuts = 0;
    for (i, row) in rows.iter().enumerate() {
        crash_at(&pristine, &journal, row.seq);
        let mut breaches = resume(&env, &id, row.seq);
        // A second crash right after the repair commit must not repair again.
        if let Some(rec) = env.journal().into_iter().find(|r| r.seq > row.seq && r.ty == "recovered") {
            repaired_cuts += 1;
            let repaired = env.dir.join("repaired.sqlite");
            let _ = std::fs::remove_file(&repaired);
            let db = rusqlite::Connection::open(&journal).unwrap();
            db.execute("VACUUM INTO ?1", [repaired.to_str().unwrap()]).unwrap();
            drop(db);
            crash_at(&repaired, &journal, rec.seq);
            breaches.extend(resume(&env, &id, rec.seq).into_iter().map(|b| format!("after repair: {b}")));
            let again = env.journal().iter().filter(|r| r.seq > rec.seq && r.ty == "recovered").count();
            if again > 0 {
                breaches.push("after repair: repaired a second time".into());
            }
        }
        if !breaches.is_empty() {
            failures.push(format!("cut after row {i} (seq {}, {}): {}", row.seq, row.ty, breaches.join("; ")));
        }
    }
    // Most cuts land mid-turn; a sweep that never repairs proves nothing.
    assert!(repaired_cuts * 2 > rows.len(), "only {repaired_cuts} of {} cuts needed a repair", rows.len());
    assert!(failures.is_empty(), "{} of {} cuts broke recovery:\n{}", failures.len(), rows.len(), failures.join("\n"));
}
