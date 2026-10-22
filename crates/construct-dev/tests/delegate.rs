//! Delegation end to end: the real binary as parent, delegating to a real
//! child process, with both on the same scripted endpoint (steps are served
//! in request order, and delegation blocks, so the order is deterministic).

mod common;

use common::*;
use serde_json::{Value, json};

/// A config with one agent that can read, its sandbox granting `allow_read`.
fn agent_config(env: &Env, extra: &str) -> String {
    let _ = env;
    format!(
        r#"
[agents.scout]
description = "reads files and reports"
prompt = "You are a scout."
tools = ["read"]
{extra}
"#
    )
}

fn delegate(task: &str) -> Value {
    json!({ "call": "delegate", "args": { "agent": "scout", "task": task } })
}

/// Journal rows of one session.
fn rows_of(env: &Env, session: &str) -> Vec<Row> {
    env.journal().into_iter().filter(|r| r.session == session).collect()
}

#[test]
fn a_delegated_task_runs_in_a_child_and_only_its_summary_returns() {
    let env = Env::new(
        "delegate",
        json!({ "steps": [
            delegate("read note.txt and say what it holds"),
            { "call": "read", "args": { "path": "note.txt" } },
            { "text": "it holds: hello" },
            { "text": "The scout says: hello." }
        ] }),
    );
    std::fs::write(env.work.join("note.txt"), "hello\n").unwrap();
    env.config(&agent_config(&env, &format!("allow_read = [\"{}\"]", env.work.display())));
    let out = env.run(&["run", "--json", "--no-learn", "ask the scout"]);
    assert!(out.status.success(), "{}", text(&out));

    // The child asked for its own prompt and only its own tools; the parent
    // got back the summary and a pointer, not the transcript.
    let reqs = env.requests();
    assert_eq!(reqs.len(), 4, "{}", text(&out));
    assert!(tool_names(&reqs[0]).contains(&"delegate".to_string()));
    assert_eq!(tool_names(&reqs[1]), vec!["read"]);
    assert_eq!(reqs[1]["messages"][0]["content"], "You are a scout.");
    let back = last_tool_result(&reqs[3]);
    assert!(back.starts_with("it holds: hello"), "{back}");
    assert!(!back.contains("1\thello"), "the child's file read stays in the child's transcript");

    // The journal: parent session, a child session linked to it by call id,
    // and the child's own rows under the child's id, written by the parent.
    let sessions = env.sessions();
    assert_eq!(sessions.len(), 2, "{sessions:?}");
    let (parent, child) = (&sessions[0], &sessions[1]);
    let child_rows = rows_of(&env, child);
    let head = &child_rows[0].payload;
    assert_eq!(head["parent_session"], json!(parent));
    let call_id = head["parent_call_id"].as_str().unwrap().to_string();
    assert!(child_rows.iter().any(|r| r.ty == "tool_result"), "the child's tool call is in the record");
    let end =
        rows_of(&env, parent).into_iter().find(|r| r.ty == "tool_end" && r.payload["name"] == "delegate").unwrap();
    assert_eq!(
        (end.payload["id"].as_str(), end.payload["data"]["session"].as_str()),
        (Some(call_id.as_str()), Some(child.as_str()))
    );
    // A child writes no journal of its own into the parent's project, and the
    // wrapper events are wire only.
    assert!(env.journal().iter().all(|r| r.ty != "child"));

    // `--json` shows the tree: the child's events wrapped with the call id.
    let wrapped: Vec<Value> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|v| v["event"] == "child")
        .collect();
    assert!(!wrapped.is_empty() && wrapped.iter().all(|v| v["id"] == json!(call_id)));
    assert!(wrapped.iter().any(|v| v["inner"]["event"] == "head"));
}

fn tool_results_of(req: &Value) -> Vec<String> {
    req["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["role"] == "tool")
        .map(|m| m["content"].as_str().unwrap_or("").to_string())
        .collect()
}

#[test]
fn a_child_with_nothing_granted_cannot_reach_the_parents_files_or_secrets() {
    let env = Env::new(
        "delegate-sandbox",
        json!({ "steps": [
            delegate("look around"),
            { "call": "read", "args": { "path": env_path("delegate-sandbox", "work/note.txt") } },
            { "call": "shell", "args": { "command": format!(
                "ls {work} 2>&1; ls {home} 2>&1; cat {journal} 2>&1; env | sort",
                work = env_path("delegate-sandbox", "work"),
                home = env_path("delegate-sandbox", "home"),
                journal = env_path("delegate-sandbox", "home/projects"),
            ) } },
            { "text": "looked" },
            { "text": "done" }
        ] }),
    );
    std::fs::write(env.work.join("note.txt"), "parent's file\n").unwrap();
    env.config(
        r#"
[agents.scout]
prompt = "You are a scout."
tools = ["read", "shell"]
"#,
    );
    let out = env.run_with(&[("SECRET_DEPLOY_TOKEN", "leak-me-please")], &["run", "--no-learn", "go"]);
    assert!(out.status.success(), "{}", text(&out));
    let reqs = env.requests();
    assert_eq!(reqs.len(), 5, "{}", text(&out));
    // Its file tool refuses the parent's working directory by name.
    assert!(last_tool_result(&reqs[2]).contains("blocked by the sandbox"), "{}", last_tool_result(&reqs[2]));
    // Its shell can't list the workspace or the parent's home, and its
    // environment holds none of the parent's secrets (nor the model key).
    let shell = last_tool_result(&reqs[3]);
    assert!(shell.matches("Permission denied").count() >= 3, "{shell}");
    assert!(!shell.contains("leak-me-please") && !shell.contains("SECRET_DEPLOY_TOKEN"), "{shell}");
    assert!(!shell.contains("OPENAI_API_KEY"), "{shell}");
    assert!(shell.contains("PATH="), "a working environment: {shell}");
}

fn env_path(name: &str, rest: &str) -> String {
    Env::dir_for(name).join(rest).display().to_string()
}

#[test]
fn a_child_cannot_delegate_remember_or_write_skills() {
    let env = Env::new(
        "delegate-denied",
        json!({ "steps": [
            delegate("try things"),
            { "calls": [
                { "call": "delegate", "args": { "agent": "scout", "task": "again" } },
                { "call": "memory", "args": { "operations": [] } },
                { "call": "skill_manage", "args": { "action": "list" } }
            ] },
            { "text": "tried" },
            { "text": "done" }
        ] }),
    );
    env.config(&agent_config(&env, ""));
    let out = env.run(&["run", "--no-learn", "go"]);
    assert!(out.status.success(), "{}", text(&out));
    let reqs = env.requests();
    assert_eq!(reqs.len(), 4, "{}", text(&out));
    assert_eq!(tool_names(&reqs[1]), vec!["read"]);
    let got = tool_results_of(&reqs[2]);
    assert_eq!(got.len(), 3);
    assert!(got.iter().all(|r| r.contains("unknown tool")), "{got:?}");
}

#[test]
fn a_definition_that_grants_a_denied_tool_stops_the_session_at_start() {
    let env = Env::new("delegate-bad-def", json!({ "steps": [ { "text": "never" } ] }));
    env.config("[agents.sneaky]\ntools = [\"memory\"]\n");
    let out = env.run(&["run", "--no-learn", "go"]);
    assert!(!out.status.success());
    assert!(text(&out).contains("may not have"), "{}", text(&out));
    assert!(env.requests().is_empty());
}

#[test]
fn the_manifest_lists_agent_definitions() {
    let env = Env::new("delegate-manifest", json!({ "steps": [] }));
    env.config(&agent_config(&env, ""));
    let out = env.run(&["manifest"]);
    let m: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(m["agents"], json!(["scout"]));
}

/// Whether the `sleep 300` a child's shell started is still running.
fn sleep_alive(work: &std::path::Path) -> bool {
    let Ok(pid) = std::fs::read_to_string(work.join("c.pid")) else { return false };
    std::fs::read(format!("/proc/{}/cmdline", pid.trim())).is_ok_and(|c| c == b"sleep\x00300\x00")
}

fn wait_for(limit_ms: u64, mut f: impl FnMut() -> bool) -> bool {
    let end = std::time::Instant::now() + std::time::Duration::from_millis(budget(limit_ms));
    while std::time::Instant::now() < end {
        if f() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    f()
}

fn sleeper_config(env: &Env) -> String {
    format!(
        r#"
[agents.scout]
prompt = "You are a scout."
tools = ["shell"]
allow_write = ["{}"]
timeout = 120
"#,
        env.work.display()
    )
}

fn sleeper_steps() -> Value {
    json!({ "steps": [
        delegate("wait around"),
        { "call": "shell", "args": { "command": "sleep 300 & echo $! > c.pid; wait" } },
        { "text": "never" }
    ] })
}

#[test]
fn killing_the_parent_leaves_no_child_running() {
    let env = Env::new("delegate-kill", sleeper_steps());
    env.config(&sleeper_config(&env));
    let mut c = std::process::Command::new(construct());
    c.args(["run", "--no-learn", "go"]).current_dir(&env.work);
    for (k, v) in env.env() {
        if v.is_empty() {
            c.env_remove(k);
        } else {
            c.env(k, v);
        }
    }
    let mut parent = c.stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn().unwrap();
    assert!(wait_for(10_000, || sleep_alive(&env.work)), "the child's command never started");
    parent.kill().unwrap(); // SIGKILL: no chance to clean up
    parent.wait().unwrap();
    assert!(wait_for(10_000, || !sleep_alive(&env.work)), "the child's sleep outlived the parent");
}

#[test]
fn cancelling_the_parent_turn_stops_the_child_and_what_it_started() {
    let env = Env::new("delegate-cancel", sleeper_steps());
    env.config(&sleeper_config(&env));
    let mut d = construct_dev::drive::Driver::spawn(
        &[construct().display().to_string()],
        &env.env(),
        Some(env.work.to_str().unwrap()),
        100,
        30,
    )
    .unwrap();
    let mut log = Vec::new();
    let (short, long) = (budget(3000), budget(8000));
    let script = format!(
        r#"
wait "ask construct" {short}
type go
key enter
wait "delegate" {long}
"#
    );
    construct_dev::drive::run_script(&mut d, &script, &mut log).unwrap_or_else(|e| panic!("{e:#}"));
    assert!(wait_for(10_000, || sleep_alive(&env.work)), "the child's command never started");
    let script = format!(
        r#"
key esc
wait "interrupted" {short}
"#
    );
    if let Err(e) = construct_dev::drive::run_script(&mut d, &script, &mut log) {
        panic!("{e:#}\n{}", String::from_utf8_lossy(&log));
    }
    assert!(wait_for(10_000, || !sleep_alive(&env.work)), "the child's sleep outlived the cancel");
    // The record: the delegate call ended interrupted, and the child's own
    // turn ended interrupted under its session.
    let rows = env.journal();
    let end = rows.iter().find(|r| r.ty == "tool_end" && r.payload["name"] == "delegate").unwrap();
    assert_eq!(end.payload["code"], "interrupted");
    let child = end.payload["data"]["session"].as_str().unwrap();
    assert!(rows.iter().any(|r| r.session == child && r.ty == "turn_end" && r.payload["interrupted"] == true));
    d.send(b"/quit\r").unwrap();
}
