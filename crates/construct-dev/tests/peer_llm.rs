//! Hub-proxied model calls (#371): a real `construct peer` with
//! `llm = "hub"` and no API key anywhere in its environment runs a tool turn
//! whose model calls all go through a real `construct hub`, which holds the
//! key and talks to the scripted model. Revoking the peer ends its model
//! access. A peer left at `llm = "local"` still calls its own endpoint.
//!
//! Unlike `peer_acp.rs`, the peer has its own home here, so nothing of the
//! hub's (config, environment) can leak into it. Every assertion reads what
//! the scripted model recorded, what the hub reports, or the file the peer's
//! tool wrote.

use crate::common::*;
use crate::hub::{command, wait_status};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn hub(env: &Env) -> Child {
    let log = std::fs::File::create(env.dir.join("hub.stderr")).unwrap();
    let child = command(env, &["hub", "--listen", "127.0.0.1:0"]).stdout(Stdio::null()).stderr(log).spawn().unwrap();
    wait_status(env, "the hub to run", |s| s["running"] == true);
    child
}

fn ok(env: &Env, args: &[&str]) -> String {
    let out = command(env, args).output().unwrap();
    assert!(out.status.success(), "{args:?} failed: {}", text(&out));
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// A peer in its own home with `config`, an environment with no model key
/// and a model URL nothing serves, joined to the hub as `old-pc`.
fn peer(env: &Env, config: &str, key: Option<&str>) -> (Child, PathBuf) {
    let home = env.dir.join("peer-home");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join("config.toml"), config).unwrap();
    let inv: Value = serde_json::from_str(&ok(env, &["hub", "invite", "old-pc", "--json"])).unwrap();
    let mut c = Command::new(construct());
    c.args(["peer", "--url", inv["url"].as_str().unwrap(), "--hub-key", inv["key"].as_str().unwrap()])
        .args(["--id", "old-pc"])
        .current_dir(&env.work);
    for (k, v) in env.env() {
        if v.is_empty() {
            c.env_remove(k);
        } else {
            c.env(k, v);
        }
    }
    // Port 9 (discard) serves nothing: a call that went there fails.
    c.env("CONSTRUCT_HOME", &home).env("CONSTRUCT_BASE_URL", "http://127.0.0.1:9/v1").env_remove("OPENAI_API_KEY");
    if let Some(k) = key {
        c.env("OPENAI_API_KEY", k).env("CONSTRUCT_BASE_URL", format!("http://127.0.0.1:{}/v1", env.port));
    }
    let log = env.dir.join("peer.stderr");
    let child = c
        .env("CONSTRUCT_JOIN", inv["token"].as_str().unwrap())
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(&log).unwrap())
        .spawn()
        .unwrap();
    wait_for(&log, "joined the hub as old-pc");
    (child, log)
}

fn wait_for(log: &Path, what: &str) {
    let deadline = Instant::now() + Duration::from_millis(budget(15_000));
    loop {
        let text = std::fs::read_to_string(log).unwrap_or_default();
        if text.contains(what) {
            return;
        }
        assert!(Instant::now() < deadline, "never saw {what:?} in {}:\n{text}", log.display());
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn wait_session(env: &Env, what: &str, done: impl Fn(&Value) -> bool) -> Value {
    let deadline = Instant::now() + Duration::from_millis(budget(20_000));
    loop {
        let out = command(env, &["hub", "session", "old-pc", "--json"]).output().unwrap();
        let v: Value = serde_json::from_slice(&out.stdout).unwrap_or(Value::Null);
        if done(&v) {
            return v;
        }
        if Instant::now() > deadline {
            let hub = std::fs::read_to_string(env.dir.join("hub.stderr")).unwrap_or_default();
            let peer = std::fs::read_to_string(env.dir.join("peer.stderr")).unwrap_or_default();
            panic!("timed out waiting for {what}; last view {v:#}\nhub:\n{hub}\npeer:\n{peer}");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn wait_exit(child: &mut Child) -> std::process::ExitStatus {
    let deadline = Instant::now() + Duration::from_millis(budget(10_000));
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        assert!(Instant::now() < deadline, "the peer never exited");
        std::thread::sleep(Duration::from_millis(50));
    }
}

const QUIET: &str = "[router]\nmode = \"local\"\n[learn]\nenabled = false\n";

#[test]
fn a_peer_without_a_key_runs_a_tool_turn_through_the_hub_and_revoking_it_ends_its_model_access() {
    let env = Env::new(
        "pllm-hub",
        json!({ "steps": [
            { "call": "shell", "args": { "command": "echo via-hub > out.txt" } },
            { "text": "done through the hub" }
        ] }),
    );
    env.config(QUIET);
    let mut hub = hub(&env);
    let (mut peer, log) = peer(&env, &format!("llm = \"hub\"\n{QUIET}"), None);
    wait_for(&log, "model calls go through the hub");

    ok(&env, &["hub", "prompt", "old-pc", "write", "the", "file"]);
    let done = wait_session(&env, "the turn to end", |v| v["last"] == "end_turn");
    assert!(done["transcript"].as_str().unwrap_or_default().contains("done through the hub"), "{done:#}");
    assert_eq!(
        std::fs::read_to_string(env.work.join("out.txt")).unwrap().trim(),
        "via-hub",
        "the tool ran on the peer"
    );

    // Both model calls reached the scripted model with the hub's key, and
    // the second carried the tool's real result.
    let reqs = env.requests();
    assert_eq!(reqs.len(), 2, "{reqs:#?}");
    assert!(env.auth_seen().iter().all(|a| a == "Bearer test…"), "the hub's key went: {:?}", env.auth_seen());
    assert!(reqs[1]["messages"].as_array().unwrap().iter().any(|m| m["role"] == "tool"), "{:#}", reqs[1]);

    // The hub counted them against the peer. A call is counted when the
    // upstream body ends, which can land just after the peer's turn ended.
    let old_pc = |s: &Value| s["peers"].as_array().and_then(|p| p.iter().find(|p| p["id"] == "old-pc")).cloned();
    let status = wait_status(&env, "both calls counted", |s| old_pc(s).is_some_and(|r| r["llm"]["requests"] == 2));
    let row = old_pc(&status).unwrap();
    assert_eq!(row["llm"]["requests"], 2, "{row:#}");

    // Revoked: the link is closed, the peer deletes its key and stops, and
    // no further model call is made on its behalf.
    ok(&env, &["hub", "revoke", "old-pc"]);
    let exit = wait_exit(&mut peer);
    assert!(!exit.success());
    wait_for(&log, "revoked");
    assert_eq!(env.requests().len(), 2);

    let _ = command(&env, &["hub", "stop"]).output();
    let _ = hub.wait();
}

#[test]
fn a_hub_killed_mid_stream_fails_the_peers_model_call_instead_of_leaving_it_hanging() {
    // The model sends a few words, then goes quiet for a minute.
    let env = Env::new("pllm-cut", json!({ "steps": [{ "text": "partial words", "stall_ms": 60_000 }] }));
    env.config(QUIET);
    let mut hub = hub(&env);
    let (mut peer, _log) = peer(&env, &format!("llm = \"hub\"\n{QUIET}"), None);
    let peer_home = env.dir.join("peer-home");
    ok(&env, &["hub", "prompt", "old-pc", "start"]);
    let deadline = Instant::now() + Duration::from_millis(budget(10_000));
    while env.requests().is_empty() {
        assert!(Instant::now() < deadline, "the call never reached the model");
        std::thread::sleep(Duration::from_millis(20));
    }
    // Let the words through the hub, then kill it mid-stream.
    std::thread::sleep(Duration::from_millis(300));
    let killed = Instant::now();
    let _ = hub.kill();
    let _ = hub.wait();

    // The peer's turn ends with an error well before the model's stall (or
    // the client's 120 s idle deadline) would have ended it.
    let deadline = Instant::now() + Duration::from_millis(budget(10_000));
    let error = loop {
        let rows = env.journal_of(&peer_home);
        if let Some(e) = rows.iter().find(|r| r.ty == "error") {
            break e.payload["message"].as_str().unwrap_or_default().to_string();
        }
        assert!(Instant::now() < deadline, "the peer's call never failed; journal: {rows:#?}");
        std::thread::sleep(Duration::from_millis(100));
    };
    assert!(killed.elapsed() < Duration::from_secs(5), "took {:?}", killed.elapsed());
    assert!(error.contains("model stream interrupted"), "{error}");

    let _ = peer.kill();
    let _ = peer.wait();
}

#[test]
fn a_local_peer_calls_its_own_endpoint_with_its_own_key() {
    let env = Env::new("pllm-local", json!({ "steps": [{ "text": "local answer" }] }));
    // The hub's own endpoint is dead: only the peer's can answer.
    env.config(&format!("base_url = \"http://127.0.0.1:9/v1\"\n{QUIET}"));
    let mut hub = {
        let log = std::fs::File::create(env.dir.join("hub.stderr")).unwrap();
        let child = command(&env, &["hub", "--listen", "127.0.0.1:0"])
            .env_remove("CONSTRUCT_BASE_URL")
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()
            .unwrap();
        wait_status(&env, "the hub to run", |s| s["running"] == true);
        child
    };
    let (mut peer, log) = peer(&env, QUIET, Some("peer-own-key"));
    ok(&env, &["hub", "prompt", "old-pc", "hello"]);
    let done = wait_session(&env, "the turn to end", |v| v["last"] == "end_turn");
    assert!(done["transcript"].as_str().unwrap_or_default().contains("local answer"), "{done:#}");
    assert!(!std::fs::read_to_string(&log).unwrap().contains("through the hub"));
    let auth = env.auth_seen();
    assert_eq!(auth.len(), 1);
    assert_eq!(auth[0], "Bearer peer…", "the peer's own key went");
    let status: Value = serde_json::from_str(&ok(&env, &["hub", "status", "--json"])).unwrap();
    assert!(status["peers"][0].get("llm").is_none(), "nothing went through the hub: {status:#}");

    let _ = peer.kill();
    let _ = peer.wait();
    let _ = command(&env, &["hub", "stop"]).output();
    let _ = hub.wait();
}

/// The peer's row in `hub status --json`.
fn row_of(env: &Env) -> Value {
    let status: Value = serde_json::from_str(&ok(env, &["hub", "status", "--json"])).unwrap();
    status["peers"].as_array().and_then(|p| p.iter().find(|p| p["id"] == "old-pc")).cloned().unwrap_or(Value::Null)
}

#[test]
fn a_model_set_at_the_hub_takes_the_peers_next_call_and_clearing_it_gives_the_peers_own_back() {
    let env = Env::new("pllm-model", json!({ "steps": [{ "text": "first answer" }, { "text": "second answer" }] }));
    env.config(&format!("{QUIET}[models.small]\ncontext_window = 200000\n"));
    let mut hub = hub(&env);
    let (mut peer, _log) = peer(&env, &format!("llm = \"hub\"\nmodel = \"big\"\n{QUIET}"), None);

    // A selector the host cannot route is refused and nothing changes.
    let out = command(&env, &["hub", "model", "old-pc", "  "]).output().unwrap();
    assert!(!out.status.success(), "{}", text(&out));
    let set = ok(&env, &["hub", "model", "old-pc", "small"]);
    assert!(set.contains("now go to small (window 200000 tokens here"), "{set}");
    assert_eq!(row_of(&env)["model"], "small", "kept on the registry row");

    ok(&env, &["hub", "prompt", "old-pc", "one"]);
    wait_session(&env, "the first turn to end", |v| v["last"] == "end_turn");
    let reqs = env.requests();
    assert_eq!(reqs[0]["model"], "small", "the hub sent the override, not the peer's selector");
    let row = row_of(&env);
    assert_eq!((row["model"].as_str(), row["asked_model"].as_str()), (Some("small"), Some("big")), "{row:#}");
    let shown = ok(&env, &["hub", "model", "old-pc"]);
    assert!(shown.contains("old-pc: small (set at the hub"), "{shown}");
    let lines = ok(&env, &["hub", "status"]);
    assert!(lines.contains("model small (set at the hub)"), "{lines}");

    // Cleared: the same running peer's next call goes to its own model.
    ok(&env, &["hub", "model", "old-pc", "--clear"]);
    ok(&env, &["hub", "prompt", "old-pc", "two"]);
    wait_session(&env, "the second turn to end", |v| {
        v["last"] == "end_turn" && v["transcript"].as_str().unwrap_or_default().contains("second answer")
    });
    let reqs = env.requests();
    assert_eq!(reqs.len(), 2, "{reqs:#?}");
    assert_eq!(reqs[1]["model"], "big");
    assert!(row_of(&env).get("model").is_none());

    let _ = peer.kill();
    let _ = peer.wait();
    let _ = command(&env, &["hub", "stop"]).output();
    let _ = hub.wait();
}

#[test]
fn an_override_too_small_for_the_peers_context_is_refused_at_the_hub_as_an_overflow_the_peer_reads() {
    let env = Env::new("pllm-window", json!({ "steps": [{ "text": "never sent" }] }));
    // No first request (system prompt and tools) fits 50 tokens.
    env.config(&format!("{QUIET}[models.tiny]\ncontext_window = 50\n"));
    let mut hub = hub(&env);
    let (mut peer, _log) = peer(&env, &format!("llm = \"hub\"\nmodel = \"big\"\n{QUIET}"), None);
    let peer_home = env.dir.join("peer-home");
    ok(&env, &["hub", "model", "old-pc", "tiny"]);
    ok(&env, &["hub", "prompt", "old-pc", "hello"]);
    wait_session(&env, "the turn to stop", |v| v["last"].is_string() && v["prompt"].is_null());

    assert!(env.requests().is_empty(), "nothing reached the model: {:#?}", env.requests());
    // The peer took the refusal as an overflow and went to compact (with
    // only the first request there is nothing to cut), rather than failing
    // on an unexplained 400.
    let rows = env.journal_of(&peer_home);
    let errors: Vec<&str> =
        rows.iter().filter(|r| r.ty == "error").filter_map(|r| r.payload["message"].as_str()).collect();
    assert_eq!(errors, ["the prompt is too long and nothing is left to cut"], "{rows:#?}");

    let _ = peer.kill();
    let _ = peer.wait();
    let _ = command(&env, &["hub", "stop"]).output();
    let _ = hub.wait();
}

#[test]
fn a_model_is_set_with_no_hub_running_and_a_running_hub_reports_it() {
    let env = Env::new("pllm-offline", json!({ "steps": [] }));
    env.config(QUIET);
    ok(&env, &["hub", "invite", "old-pc"]);
    assert!(ok(&env, &["hub", "model", "old-pc", "small"]).contains("now go to small"));
    let out = command(&env, &["hub", "model", "nobody", "small"]).output().unwrap();
    assert!(!out.status.success() && text(&out).contains("no peer nobody"), "{}", text(&out));
    let mut hub = hub(&env);
    assert_eq!(row_of(&env)["model"], "small");
    let _ = command(&env, &["hub", "stop"]).output();
    let _ = hub.wait();
}
