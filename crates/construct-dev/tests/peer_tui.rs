//! Peers in the TUI (#365): a real `construct hub`, a real `construct peer`
//! joining through the pairing window, and the TUI in a pty on the same
//! home. The person approves the join by the code on screen and denies the
//! peer's permission question from the PEER popup. What is asserted is what
//! they see, the hub's registry, and the peer's journal.
//!
//! As in `peer_acp`, the hub and the peer share the env's home; the TUI's
//! session never calls the model, so every journal row about a tool is the
//! peer's.

use crate::common::*;
use crate::hub::command;
use crate::peer_acp::{free_port, hub_on, ok, peer_log, wait_log};
use construct_dev::drive;
use serde_json::{Value, json};
use std::process::Stdio;

fn play(d: &mut drive::Driver, script: &str) -> String {
    let mut log = Vec::new();
    if let Err(e) = drive::run_script(d, script, &mut log) {
        panic!("{e:#}\n{}", String::from_utf8_lossy(&log));
    }
    String::from_utf8_lossy(&log).to_string()
}

#[test]
fn a_join_is_approved_by_its_code_and_a_peer_question_is_answered_from_its_own_popup() {
    let outside = Env::dir_for("peer-tui").join("outside/secret.txt");
    let env = Env::new(
        "peer-tui",
        json!({ "steps": [
            { "call": "read", "args": { "path": outside.display().to_string() } },
            { "text": "peer finished" }
        ] }),
    );
    env.config("[router]\nmode = \"local\"\n[learn]\nenabled = false\n");
    std::fs::create_dir_all(outside.parent().unwrap()).unwrap();
    std::fs::write(&outside, "secret value\n").unwrap();
    let mut hub = hub_on(&env, free_port());
    ok(&env, &["hub", "pair", "--for", "10m"]);
    let url = ok(&env, &["hub", "status", "--json"]);
    let status: Value = serde_json::from_str(&url).unwrap();
    let log = std::fs::File::create(peer_log(&env)).unwrap();
    let mut peer = command(&env, &["peer", "--id", "old-pc"])
        .args(["--url", status["listen"].as_str().unwrap(), "--hub-key", status["key"].as_str().unwrap()])
        .stdout(Stdio::null())
        .stderr(log)
        .spawn()
        .unwrap();
    wait_log(&env, "waiting for approval", 1);
    let printed = std::fs::read_to_string(peer_log(&env)).unwrap();
    let code = printed.split("code ").nth(1).and_then(|r| r.split_whitespace().next()).unwrap().to_string();

    let mut d = drive::Driver::spawn(
        &[construct().display().to_string()],
        &env.env(),
        Some(env.work.to_str().unwrap()),
        110,
        34,
    )
    .unwrap();
    let screen = play(
        &mut d,
        &format!(
            "wait \"ask construct\" 5000\nwait \"old-pc pending {code}\" 8000\nwait \"wants to join this hub\" 3000\n\
             expect \"code  {code}\"\ntype a\nsleep 100\nexpect \"wants to join this hub\"\nsleep 600\ntype a\nwait \"approved: its key is pinned\" 5000\n\
             wait \"old-pc online\" 5000\ntype /peer old-pc read the secret\nkey enter\n\
             wait \"about its own machine\" 15000\nexpect \"Always allow · saved to old-pc's config\"\n\
             sleep 600\ntype n\nwait \"answered\" 5000\ngone \"about its own machine\" 3000\nsnap done\n"
        ),
    );
    assert!(screen.contains("old-pc: answered \"deny\""), "{screen}");

    // The registry holds the pin; the peer's read was refused and never ran.
    let reg: Value = serde_json::from_slice(&std::fs::read(env.home.join("hub/peers.json")).unwrap()).unwrap();
    assert_eq!(reg["peers"][0]["state"], "pinned", "{reg}");
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(budget(10_000));
    let ends = loop {
        let ends: Vec<Value> = env.journal().into_iter().filter(|r| r.ty == "tool_end").map(|r| r.payload).collect();
        if !ends.is_empty() || std::time::Instant::now() > deadline {
            break ends;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    assert_eq!(ends.len(), 1, "{ends:?}");
    assert_eq!(ends[0]["ok"], false, "{:?}", ends[0]);
    assert!(!ends[0]["output"].as_str().unwrap_or_default().contains("secret value"));
    let access: Vec<Value> = env.journal().into_iter().filter(|r| r.ty == "access").map(|r| r.payload).collect();
    assert!(access.iter().any(|a| a["decision"] == "deny"), "{access:?}");

    d.send(b"/quit\r").unwrap();
    let _ = peer.kill();
    let _ = peer.wait();
    let _ = command(&env, &["hub", "stop"]).output();
    let _ = hub.wait();
}
