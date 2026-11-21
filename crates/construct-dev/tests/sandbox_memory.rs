//! The memory ceiling on a shell call (#230, ADR 0061): a command that keeps
//! allocating past `[sandbox] memory_max` is killed by the sandbox helper,
//! the model is told why in prose, and construct carries on with the turn.

use crate::common::*;
use serde_json::json;

/// Doubles a string to 64 MiB (bash holds a copy or two), then holds it long
/// enough for any sample to see it. Bounded, so a broken ceiling costs this
/// test a few hundred MB, not the machine.
const GROW: &str = "x=a; for i in $(seq 1 26); do x=$x$x; done; sleep 5; echo SURVIVED";

#[test]
fn a_command_past_the_memory_ceiling_is_killed_and_the_model_told_why() {
    let env = Env::new(
        "memory-ceiling",
        json!({ "steps": [
            { "call": "shell", "args": { "command": GROW } },
            { "call": "shell", "args": { "command": "echo still-here" } },
            { "text": "done" }
        ] }),
    );
    env.config("[sandbox]\nmemory_max = \"16M\"\n");
    let out = env.run(&["run", "--no-learn", "grow a string"]);
    assert!(out.status.success(), "construct survived the kill: {}", text(&out));

    let last = env.requests().pop().unwrap();
    let results: Vec<String> = last["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["role"] == "tool")
        .map(|m| m["content"].as_str().unwrap_or("").to_string())
        .collect();
    assert_eq!(results.len(), 2, "{results:#?}");
    let killed = &results[0];
    assert!(!killed.contains("SURVIVED"), "{killed}");
    assert!(killed.contains("over the sandbox's 16.0 MiB limit ([sandbox] memory_max)"), "{killed}");
    assert!(killed.contains("do not rerun it as is"), "{killed}");
    assert!(!killed.contains("[killed by signal]"), "the cause, not the signal: {killed}");
    assert_eq!(results[1].trim(), "still-here", "the next call runs normally");

    let ends: Vec<Row> = env.journal().into_iter().filter(|r| r.ty == "tool_end").collect();
    assert_eq!(ends[0].payload["ok"], false);
    assert_eq!(ends[0].payload["code"], "memory", "{}", ends[0].payload);
    let ms = ends[0].payload["ms"].as_u64().unwrap();
    assert!(ms < budget(4000), "killed while growing, not after its sleep: {ms} ms");
    assert_eq!(ends[1].payload["ok"], true);
}

#[test]
fn the_manifest_shows_the_ceiling_and_off_removes_it() {
    let env = Env::new("memory-manifest", json!({ "steps": [] }));
    env.config("[sandbox]\nmemory_max = \"2G\"\n");
    let m: serde_json::Value = serde_json::from_slice(&env.run(&["manifest"]).stdout).unwrap();
    assert_eq!(m["memory_max"], "2G");
    assert_eq!(m["memory_max_bytes"], 2u64 << 30);

    let off = env.run_with(&[("CONSTRUCT_SANDBOX_MEMORY_MAX", "off")], &["manifest"]);
    let m: serde_json::Value = serde_json::from_slice(&off.stdout).unwrap();
    assert_eq!(m["memory_max"], "off");
    assert!(m.get("memory_max_bytes").is_none(), "{m}");

    let bad = env.run_with(&[("CONSTRUCT_SANDBOX_MEMORY_MAX", "lots")], &["manifest"]);
    assert!(!bad.status.success());
    assert!(text(&bad).contains("memory_max"), "{}", text(&bad));
}
