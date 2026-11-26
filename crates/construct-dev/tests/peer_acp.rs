//! Remote sessions (#362): a real `construct hub` drives a real `construct
//! peer`'s agent over the peer link's `acp` channel, with a scripted model on
//! the peer. The hub is killed or stopped mid-turn and started again on the
//! same address, while the peer keeps working. Every assertion reads what
//! the hub reports (`hub session --json`), the peer's journal, the requests
//! its model got, or the files its tools wrote, never only what was printed.
//!
//! The hub and the peer share one home here: their files do not overlap
//! (`hub.*`, `hub/` against `peer.key` and the journal), and it lets
//! `Env::journal` read the peer's journal.

use crate::common::*;
use crate::hub::{command, wait_status};
use serde_json::{Value, json};
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

/// A loopback port nothing listens on, so a restarted hub can take it again.
pub(crate) fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

pub(crate) fn hub_on(env: &Env, port: u16) -> Child {
    let log = std::fs::OpenOptions::new().create(true).append(true).open(env.dir.join("hub.stderr")).unwrap();
    let child = command(env, &["hub", "--listen", &format!("127.0.0.1:{port}")])
        .stdout(Stdio::null())
        .stderr(log)
        .spawn()
        .unwrap();
    wait_status(env, "the hub to run", |s| s["running"] == true);
    child
}

pub(crate) fn ok(env: &Env, args: &[&str]) -> String {
    let out = command(env, args).output().unwrap();
    assert!(out.status.success(), "{args:?} failed: {}", text(&out));
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// Invite `old-pc` and start it with the token; returns once it joined.
fn join(env: &Env) -> Child {
    let inv: Value = serde_json::from_str(&ok(env, &["hub", "invite", "old-pc", "--json"])).unwrap();
    let args = ["peer", "--url", inv["url"].as_str().unwrap(), "--hub-key", inv["key"].as_str().unwrap()];
    let log = std::fs::File::create(peer_log(env)).unwrap();
    let child = command(env, &args)
        .args(["--id", "old-pc"])
        .env("CONSTRUCT_JOIN", inv["token"].as_str().unwrap())
        .stdout(Stdio::null())
        .stderr(log)
        .spawn()
        .unwrap();
    wait_log(env, "joined the hub as old-pc", 1);
    child
}

pub(crate) fn peer_log(env: &Env) -> std::path::PathBuf {
    env.dir.join("peer.stderr")
}

/// Wait until the peer printed `what` at least `times` times.
pub(crate) fn wait_log(env: &Env, what: &str, times: usize) {
    let deadline = Instant::now() + Duration::from_millis(budget(15_000));
    loop {
        let log = std::fs::read_to_string(peer_log(env)).unwrap_or_default();
        if log.matches(what).count() >= times {
            return;
        }
        assert!(Instant::now() < deadline, "the peer never printed {what:?} {times}x; it printed:\n{log}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

pub(crate) fn session(env: &Env) -> Value {
    serde_json::from_str(&ok(env, &["hub", "session", "old-pc", "--json"])).unwrap()
}

/// Poll the hub's view of the session until `done` holds.
pub(crate) fn wait_session(env: &Env, what: &str, done: impl Fn(&Value) -> bool) -> Value {
    let deadline = Instant::now() + Duration::from_millis(budget(20_000));
    loop {
        let out = command(env, &["hub", "session", "old-pc", "--json"]).output().unwrap();
        let v: Value = serde_json::from_slice(&out.stdout).unwrap_or(Value::Null);
        if done(&v) {
            return v;
        }
        if Instant::now() > deadline {
            let hub = std::fs::read_to_string(env.dir.join("hub.stderr")).unwrap_or_default();
            let peer = std::fs::read_to_string(peer_log(env)).unwrap_or_default();
            panic!("timed out waiting for {what}; last view {v:#}\nhub:\n{hub}\npeer:\n{peer}");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn wait_file(path: &std::path::Path) {
    let deadline = Instant::now() + Duration::from_millis(budget(10_000));
    while !path.exists() {
        assert!(Instant::now() < deadline, "{} never appeared", path.display());
        std::thread::sleep(Duration::from_millis(15));
    }
}

pub(crate) fn rows(env: &Env, ty: &str) -> Vec<Value> {
    env.journal().into_iter().filter(|r| r.ty == ty).map(|r| r.payload).collect()
}

fn stop(env: &Env, mut peer: Child, mut hub: Child) {
    let _ = peer.kill();
    let _ = peer.wait();
    let _ = command(env, &["hub", "stop"]).output();
    let _ = hub.wait();
}

#[test]
fn two_prompts_run_in_one_session_and_a_hub_killed_mid_turn_reattaches_without_running_it_twice() {
    let env = Env::new(
        "peer-kill",
        json!({ "steps": [
            { "text": "first answer" },
            { "call": "shell", "args": { "command": "echo ran >> effects.txt; sleep 5" } },
            { "text": "second answer" }
        ] }),
    );
    env.config("[router]\nmode = \"local\"\n[learn]\nenabled = false\n");
    let port = free_port();
    let hub = hub_on(&env, port);
    let peer = join(&env);

    // The first prompt opens the session in the directory the peer reported.
    let waited = ok(&env, &["hub", "prompt", "old-pc", "say", "one", "--wait"]);
    assert!(waited.contains("first answer"), "{waited}");
    let one = session(&env);
    let sid = one["session"].as_str().unwrap().to_string();
    assert_eq!(one["cwd"], env.work.display().to_string());
    assert_eq!((one["turn"]["turn"].as_u64(), one["last"].as_str()), (Some(1), Some("end_turn")), "{one:#}");

    // The second runs a slow tool; the hub dies once its effect is real.
    ok(&env, &["hub", "prompt", "old-pc", "say", "two"]);
    wait_file(&env.work.join("effects.txt"));
    let mut hub = hub;
    hub.kill().unwrap();
    let _ = hub.wait();
    let row = |env: &Env| {
        let reg: Value = serde_json::from_slice(&std::fs::read(env.home.join("hub/peers.json")).unwrap()).unwrap();
        reg["peers"][0].clone()
    };
    let kept = row(&env);
    assert_eq!(kept["session"], sid.as_str(), "the session survives the hub: {kept}");
    assert_eq!(kept["prompt"]["text"], "say two", "so does the unanswered prompt: {kept}");
    let key = kept["prompt"]["key"].as_str().unwrap().to_string();

    // A new hub on the same address: the peer redials, the hub reloads the
    // session, resends the prompt with its key, and sees the turn finish.
    let hub = hub_on(&env, port);
    wait_log(&env, "joined the hub as old-pc", 2);
    let live = wait_session(&env, "the reattach", |v| v["attached"] == true && v["turn"]["turn"] == 2);
    assert_eq!(live["turn"]["state"], "working", "reattached mid-turn, the turn still running: {live:#}");
    assert!(live["transcript"].as_str().unwrap().contains("first answer"), "{live:#}");
    let done = wait_session(&env, "the second turn to finish", |v| v["prompt"].is_null() && v["last"].is_string());
    assert_eq!(done["session"], sid.as_str(), "the same session: {done:#}");
    assert_eq!(done["last"], "end_turn", "{done:#}");
    assert_eq!(done["turn"]["turn"], 2, "{done:#}");
    let transcript = done["transcript"].as_str().unwrap();
    for want in ["> say one", "first answer", "> say two", "[completed]", "second answer"] {
        assert!(transcript.contains(want), "the reloaded transcript lacks {want:?}:\n{transcript}");
    }

    // The peer's turn ran once: one effect, one model call per step, one
    // `submitted` row per prompt (the second with the hub's key), and no
    // recovery, because the peer process never stopped.
    assert_eq!(std::fs::read_to_string(env.work.join("effects.txt")).unwrap(), "ran\n");
    assert_eq!(env.requests().len(), 3, "turn one: 1 call, turn two: 2");
    let submitted = rows(&env, "submitted");
    assert_eq!(submitted.len(), 2, "{submitted:?}");
    assert_eq!(submitted[1]["idempotency_key"], key.as_str());
    assert!(rows(&env, "recovered").is_empty());
    assert!(rows(&env, "turn_end").iter().all(|t| t["interrupted"] != true));
    assert!(row(&env).get("prompt").is_none(), "the answered prompt is forgotten");
    stop(&env, peer, hub);
}

#[test]
fn a_permission_question_reaches_the_hub_survives_a_restart_and_its_answer_reaches_the_peer() {
    let outside = Env::dir_for("peer-ask").join("outside/secret.txt");
    let env = Env::new(
        "peer-ask",
        json!({ "steps": [
            { "call": "read", "args": { "path": outside.display().to_string() } },
            { "text": "done" }
        ] }),
    );
    env.config("[router]\nmode = \"local\"\n[learn]\nenabled = false\n");
    std::fs::create_dir_all(outside.parent().unwrap()).unwrap();
    std::fs::write(&outside, "secret value\n").unwrap();
    let port = free_port();
    let hub = hub_on(&env, port);
    let peer = join(&env);

    ok(&env, &["hub", "prompt", "old-pc", "read", "the", "secret"]);
    let asked = wait_session(&env, "a question", |v| v["questions"].as_array().is_some_and(|q| !q.is_empty()));
    let q = &asked["questions"][0];
    assert!(q["title"].as_str().unwrap().contains("secret.txt"), "{q}");
    assert!(q["options"].as_array().unwrap().iter().any(|o| o == "once"), "{q}");
    assert_eq!(asked["turn"]["state"], "waiting");

    // A graceful stop this time: the peer is told, and keeps waiting.
    ok(&env, &["hub", "stop"]);
    let mut hub = hub;
    let _ = hub.wait();
    let hub = hub_on(&env, port);
    wait_log(&env, "joined the hub as old-pc", 2);
    let again =
        wait_session(&env, "the question reissued", |v| v["questions"].as_array().is_some_and(|q| q.len() == 1));
    assert_eq!(again["questions"][0], *q, "the same question, reissued on load");
    assert_eq!(again["turn"]["state"], "waiting");
    assert!(rows(&env, "tool_end").is_empty(), "nothing ran while the hub was away");

    // A wrong option is refused; the answer reaches the peer and the read runs.
    let bad = command(&env, &["hub", "answer", "old-pc", "sometimes"]).output().unwrap();
    assert!(!bad.status.success() && text(&bad).contains("not offered"), "{}", text(&bad));
    ok(&env, &["hub", "answer", "old-pc", "once"]);
    let done = wait_session(&env, "the turn to finish", |v| v["prompt"].is_null() && v["last"].is_string());
    assert_eq!(done["last"], "end_turn", "{done:#}");
    assert!(done["questions"].as_array().unwrap().is_empty());
    let ends = rows(&env, "tool_end");
    assert_eq!(ends.len(), 1, "{ends:?}");
    assert_eq!(ends[0]["ok"], true);
    assert!(ends[0]["output"].as_str().unwrap().contains("secret value"));
    assert_eq!(rows(&env, "submitted").len(), 1, "the resent prompt ran once");
    stop(&env, peer, hub);
}
