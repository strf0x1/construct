//! A peer survives reboots and crashes (#367, ADR 0076): a real `construct
//! hub` and a real `construct peer` run the way the installed unit runs it
//! (`--require-key`, no token). The peer is SIGKILLed mid-turn and started
//! again, revoked while it is down, or started before its hub. Assertions
//! read what the hub reports (`hub session --json`), the peer's journal and
//! exit code, the requests its model got, and the files it leaves.
//!
//! The hub and the peer share one home, as in `peer_acp.rs`, so
//! `Env::journal` reads the peer's journal.

use crate::common::*;
use crate::hub::command;
use crate::peer_acp::{free_port, hub_on, ok, peer_log, rows, wait_log, wait_session};
use serde_json::{Value, json};
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

/// The exit code the installed unit does not restart (`service::EXIT_NO_RESTART`).
const NO_RESTART: i32 = 77;

/// `construct peer` as the unit runs it, dialing `inv`'s hub; stderr appends
/// to `peer_log`, so `wait_log` counts across restarts.
fn peer(env: &Env, inv: &Value, token: Option<&str>) -> std::process::Command {
    let mut c = command(env, &["peer", "--require-key", "--id", "old-pc"]);
    c.args(["--url", inv["url"].as_str().unwrap(), "--hub-key", inv["key"].as_str().unwrap()]);
    match token {
        Some(t) => c.env("CONSTRUCT_JOIN", t),
        None => c.env_remove("CONSTRUCT_JOIN"),
    };
    let log = std::fs::OpenOptions::new().create(true).append(true).open(peer_log(env)).unwrap();
    c.stdout(Stdio::null()).stderr(log);
    c
}

/// Invite `old-pc` and join it once with the token, the way a drop starts.
/// `--require-key` needs a key, so the first run is a plain one.
fn first_join(env: &Env) -> (Value, Child) {
    let inv: Value = serde_json::from_str(&ok(env, &["hub", "invite", "old-pc", "--json"])).unwrap();
    let mut c = command(env, &["peer", "--id", "old-pc"]);
    c.args(["--url", inv["url"].as_str().unwrap(), "--hub-key", inv["key"].as_str().unwrap()]);
    let log = std::fs::OpenOptions::new().create(true).append(true).open(peer_log(env)).unwrap();
    let child =
        c.env("CONSTRUCT_JOIN", inv["token"].as_str().unwrap()).stdout(Stdio::null()).stderr(log).spawn().unwrap();
    wait_log(env, "joined the hub as old-pc", 1);
    (inv, child)
}

fn exits(env: &Env, child: &mut Child) -> std::process::ExitStatus {
    let deadline = Instant::now() + Duration::from_millis(budget(15_000));
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        assert!(Instant::now() < deadline, "the peer did not exit:\n{}", log(&peer_log(env)));
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn log(path: &std::path::Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

fn kill(mut child: Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn stop_hub(env: &Env, mut hub: Child) {
    let _ = command(env, &["hub", "stop"]).output();
    let _ = hub.wait();
}

#[test]
fn a_peer_killed_mid_turn_rejoins_without_a_token_and_the_hub_sees_the_turn_cut_off_once() {
    let env = Env::new(
        "peer-crash",
        json!({ "steps": [
            { "call": "shell", "args": { "command": "echo ran >> effects.txt; sleep 30" } },
            { "text": "never sent" }
        ] }),
    );
    env.config("[router]\nmode = \"local\"\n[learn]\nenabled = false\n");
    let hub = hub_on(&env, free_port());
    let (inv, first) = first_join(&env);
    let key = std::fs::read(env.home.join("peer.key")).unwrap();

    ok(&env, &["hub", "prompt", "old-pc", "do", "the", "slow", "thing"]);
    let effects = env.work.join("effects.txt");
    let deadline = Instant::now() + Duration::from_millis(budget(15_000));
    while !effects.exists() {
        assert!(Instant::now() < deadline, "the tool never ran:\n{}", log(&peer_log(&env)));
        std::thread::sleep(Duration::from_millis(15));
    }
    // A crash: no signal handler runs, nothing is cancelled cleanly.
    kill(first);

    // The unit's restart: no token, the same key, no approval.
    let again = peer(&env, &inv, None).spawn().unwrap();
    wait_log(&env, "joined the hub as old-pc", 2);
    assert_eq!(std::fs::read(env.home.join("peer.key")).unwrap(), key, "the same key, reloaded");
    let done = wait_session(&env, "the resent prompt's answer", |v| v["prompt"].is_null() && v["last"].is_string());
    assert_eq!(done["last"], "cancelled", "cut off by the crash, answered once: {done:#}");
    assert!(done["transcript"].as_str().unwrap().contains("do the slow thing"), "{done:#}");

    // It ran once and was never re-run: one effect, one model call, one
    // `submitted` row, and recovery marked the turn interrupted.
    assert_eq!(std::fs::read_to_string(&effects).unwrap(), "ran\n");
    assert_eq!(env.requests().len(), 1, "no model call after the restart");
    assert_eq!(rows(&env, "submitted").len(), 1);
    assert!(!rows(&env, "recovered").is_empty(), "recovery ran on load");
    let st: Value = serde_json::from_str(&ok(&env, &["hub", "status", "--json"])).unwrap();
    let row = st["peers"].as_array().unwrap().iter().find(|p| p["id"] == "old-pc").unwrap().clone();
    assert_eq!(row["state"], "pinned", "{row}");
    let hub_log = log(&env.dir.join("hub.stderr"));
    assert_eq!(hub_log.matches("old-pc connected").count(), 2, "{hub_log}");
    assert!(!hub_log.contains("wants to join"), "no approval asked: {hub_log}");
    kill(again);
    stop_hub(&env, hub);
}

#[test]
fn a_peer_revoked_while_down_deletes_its_key_and_exits_with_the_no_restart_code() {
    let env = Env::new("pr-revoked", json!({ "steps": [] }));
    let hub = hub_on(&env, free_port());
    let (inv, first) = first_join(&env);
    kill(first);
    ok(&env, &["hub", "revoke", "old-pc"]);

    let mut back = peer(&env, &inv, None).spawn().unwrap();
    let status = exits(&env, &mut back);
    assert_eq!(status.code(), Some(NO_RESTART), "{}", log(&peer_log(&env)));
    assert!(log(&peer_log(&env)).contains("the hub revoked old-pc"), "{}", log(&peer_log(&env)));
    assert!(!env.home.join("peer.key").exists(), "the revoked key is deleted");

    // Started again (a unit without RestartPreventExitStatus, or by hand):
    // no key, so it needs a new join and makes no key of its own.
    let out = peer(&env, &inv, None).stderr(Stdio::piped()).output().unwrap();
    assert_eq!(out.status.code(), Some(NO_RESTART));
    assert!(text(&out).contains("needs a new join"), "{}", text(&out));
    assert!(!env.home.join("peer.key").exists(), "--require-key never creates a key");
    stop_hub(&env, hub);
}

#[test]
fn an_installed_peer_started_before_its_hub_waits_for_it() {
    let env = Env::new("pr-boot", json!({ "steps": [] }));
    let port = free_port();
    let hub = hub_on(&env, port);
    let (inv, first) = first_join(&env);
    kill(first);
    stop_hub(&env, hub);

    // Booted with the hub still down: it keeps dialing instead of exiting.
    let mut early = peer(&env, &inv, None).spawn().unwrap();
    wait_log(&env, "cannot reach the hub", 2);
    assert!(early.try_wait().unwrap().is_none(), "it gave up:\n{}", log(&peer_log(&env)));
    let hub = hub_on(&env, port);
    wait_log(&env, "joined the hub as old-pc", 2);
    kill(early);
    stop_hub(&env, hub);
}
