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

/// What each request's `Authorization` header looked like, in arrival order:
/// null, or the scheme and the token's first four characters.
fn auth_seen(env: &Env) -> Vec<Value> {
    let log = std::fs::read_to_string(env.dir.join("log.jsonl")).unwrap();
    log.lines().map(|l| serde_json::from_str::<Value>(l).unwrap()["auth"].clone()).collect()
}

#[test]
fn a_child_on_its_own_endpoint_gets_its_own_key_or_none() {
    for (name, extra, child_auth) in [
        ("delegate-key-none", "", Value::Null),
        ("delegate-key-named", "api_key_secret = \"CHILD_KEY\"\n", json!("Bearer chil…")),
    ] {
        let env = Env::new(name, json!({ "steps": [delegate("hi"), { "text": "hello" }, { "text": "done" }] }));
        let base = format!("base_url = \"http://127.0.0.1:{}/v1\"\n{extra}", env.port);
        env.config(&agent_config(&env, &base));
        let out = env
            .run_with(&[("OPENAI_API_KEY", "parent-AAAA"), ("CHILD_KEY", "child-BBBB")], &["run", "--no-learn", "go"]);
        assert!(out.status.success(), "{}", text(&out));
        // Parent, child, parent: the parent's key never reaches the child.
        assert_eq!(auth_seen(&env), vec![json!("Bearer pare…"), child_auth, json!("Bearer pare…")], "{name}");
    }
}

/// The first scenario above, on either transport: what the child's journal
/// rows were (types, in order) and what the parent got back.
fn scout_run(name: &str, protocol: &str) -> (Vec<String>, String, Vec<Row>) {
    let env = Env::new(
        name,
        json!({ "steps": [
            delegate("read note.txt and say what it holds"),
            { "call": "read", "args": { "path": "note.txt" } },
            { "text": "it holds: hello" },
            { "text": "The scout says: hello." }
        ] }),
    );
    std::fs::write(env.work.join("note.txt"), "hello\n").unwrap();
    let extra = format!("protocol = \"{protocol}\"\nallow_read = [\"{}\"]", env.work.display());
    env.config(&agent_config(&env, &extra));
    let out = env.run(&["run", "--json", "--no-learn", "ask the scout"]);
    assert!(out.status.success(), "{}", text(&out));
    let sessions = env.sessions();
    assert_eq!(sessions.len(), 2, "{sessions:?}");
    let rows = rows_of(&env, &sessions[1]);
    let types = rows.iter().map(|r| r.ty.clone()).collect();
    let back = last_tool_result(env.requests().last().unwrap());
    let parent_end = rows_of(&env, &sessions[0]).into_iter().filter(|r| r.ty == "tool_end").collect();
    (types, back, parent_end)
}

#[test]
fn construct_over_acp_journals_the_same_rows_as_construct_over_jsonl() {
    let (jsonl_rows, jsonl_back, _) = scout_run("delegate-jsonl", "jsonl");
    let (acp_rows, acp_back, parent_end) = scout_run("delegate-acp", "acp");
    assert!(jsonl_rows.iter().any(|t| t == "tool_result") && jsonl_rows.iter().any(|t| t == "usage"), "{jsonl_rows:?}");
    assert_eq!(acp_rows, jsonl_rows);
    assert!(acp_back.starts_with("The scout says: hello.") || acp_back.starts_with("it holds: hello"), "{acp_back}");
    // Same shape of answer on both wires.
    assert_eq!(acp_back.split("\n[full").next(), jsonl_back.split("\n[full").next());
    let end = parent_end.iter().find(|r| r.payload["name"] == "delegate").unwrap();
    assert_eq!(end.payload["ok"], true);
    assert!(end.payload["data"]["session"].is_string());
}

/// A parent that delegates once to the scripted foreign ACP agent `ext`.
fn ext_run(name: &str, extra: &str, fake_args: &[&str]) -> (Env, std::process::Output) {
    let env = Env::new(
        name,
        json!({ "steps": [
            { "call": "delegate", "args": { "agent": "ext", "task": "clean the build dir" } },
            { "text": "the agent finished" }
        ] }),
    );
    let args: Vec<String> =
        std::iter::once("acp-fake").chain(fake_args.iter().copied()).map(|a| format!("{a:?}")).collect();
    // The agent's install path: the parent may read it, and so may the child.
    let bin = dev_bin();
    let dir = bin.parent().unwrap().display();
    env.config(&format!(
        "[sandbox]\nallow_read = [\"{dir}\"]\n[agents.ext]\ndescription = \"a foreign agent\"\nprotocol = \"acp\"\nallow_read = [\"{dir}\"]\ncommand = [\"{}\", {}]\n{extra}\n",
        bin.display(),
        args.join(", ")
    ));
    let out = env.run(&["run", "--json", "--no-learn", "ask ext"]);
    (env, out)
}

fn ext_rows(env: &Env) -> (String, Vec<Row>) {
    let sessions = env.sessions();
    assert_eq!(sessions.len(), 2, "{sessions:?}");
    (sessions[1].clone(), rows_of(env, &sessions[1]))
}

#[test]
fn a_foreign_acp_agents_permission_request_is_rejected_by_default_and_journaled() {
    let (env, out) = ext_run("acp-ext-reject", "", &[]);
    assert!(out.status.success(), "{}", text(&out));
    let (child, rows) = ext_rows(&env);
    // The child's session id is the ACP session id, linked to the parent's call.
    assert_eq!(child, "fake-session-1");
    assert_eq!(rows[0].ty, "head");
    assert_eq!(rows[0].payload["harness"], "ext");
    assert_eq!(rows[0].payload["version"], "9.9");
    assert!(rows[0].payload["parent_call_id"].is_string());
    let types: Vec<&str> = rows.iter().map(|r| r.ty.as_str()).collect();
    assert_eq!(types, ["head", "user", "turn_start", "tool_start", "permission", "tool_end", "assistant", "turn_end"]);
    // The call is journaled once, with the input that arrived after it opened.
    assert_eq!(rows[3].payload["name"], "bash");
    assert!(rows[3].payload["args"].as_str().unwrap().contains("rm -rf build"), "{}", rows[3].payload);
    assert_eq!(rows[5].payload["name"], "bash");
    let usage = &rows[7].payload;
    assert_eq!(
        (usage["prompt"].as_u64(), usage["completion"].as_u64(), usage["cached"].as_u64()),
        (Some(11), Some(7), Some(5))
    );
    let p = &rows[4].payload;
    assert_eq!((p["decision"].as_str(), p["title"].as_str()), (Some("reject"), Some("rm -rf build")));
    assert_eq!(p["options"].as_array().unwrap().len(), 4);
    // The answer reached the child: it reports the option it was given.
    assert_eq!(rows[5].payload["output"], "permission: opt-reject_once");
    assert_eq!(rows[5].payload["ok"], false);
    let back = last_tool_result(env.requests().last().unwrap());
    assert!(back.starts_with("done: opt-reject_once"), "{back}");
    let end = env.journal().into_iter().find(|r| r.ty == "tool_end" && r.payload["name"] == "delegate").unwrap();
    assert_eq!(end.payload["data"]["session"], "fake-session-1");
}

#[test]
fn on_permission_allow_answers_with_an_allow_option() {
    let (env, out) = ext_run("acp-ext-allow", "on_permission = \"allow\"", &[]);
    assert!(out.status.success(), "{}", text(&out));
    let (_, rows) = ext_rows(&env);
    let perm = rows.iter().find(|r| r.ty == "permission").unwrap();
    assert_eq!(perm.payload["decision"], "allow");
    let back = last_tool_result(env.requests().last().unwrap());
    assert!(back.starts_with("done: opt-allow_once"), "{back}");
}

#[test]
fn with_no_option_of_the_wanted_kind_the_request_is_cancelled_not_allowed() {
    let (env, out) = ext_run("acp-ext-nokind", "", &["--kinds", "allow_once,allow_always"]);
    assert!(out.status.success(), "{}", text(&out));
    let (_, rows) = ext_rows(&env);
    assert_eq!(rows.iter().find(|r| r.ty == "permission").unwrap().payload["decision"], "cancelled");
    let back = last_tool_result(env.requests().last().unwrap());
    assert!(back.starts_with("done: cancelled"), "{back}");
}

#[test]
fn what_construct_does_not_advertise_is_refused_as_an_unknown_method() {
    let (env, out) = ext_run("acp-ext-fs", "", &["--ask-fs"]);
    assert!(out.status.success(), "{}", text(&out));
    let back = last_tool_result(env.requests().last().unwrap());
    assert!(back.contains("fs error -32601"), "{back}");
}

#[test]
fn a_timeout_sends_session_cancel_and_the_agent_ends_its_turn() {
    let (env, out) = ext_run("acp-ext-timeout", "timeout = 2", &["--wait-for-cancel"]);
    assert!(out.status.success(), "{}", text(&out));
    let end = env.journal().into_iter().find(|r| r.ty == "tool_end" && r.payload["name"] == "delegate").unwrap();
    assert_eq!(end.payload["ok"], false);
    assert_eq!(end.payload["code"], "timeout");
    // It wrapped up on the cancel rather than being killed: its turn ended.
    let (_, rows) = ext_rows(&env);
    let last = rows.last().unwrap();
    assert_eq!((last.ty.as_str(), last.payload["interrupted"].as_bool()), ("turn_end", Some(true)));
}

#[test]
fn a_child_with_network_granted_still_starts_under_the_sandbox() {
    // `network = true` leaves TCP unhandled by the ruleset; a port exception
    // for the model endpoint on top of that used to stop the helper at exec.
    let env =
        Env::new("delegate-network", json!({ "steps": [delegate("say hi"), { "text": "hi" }, { "text": "done" }] }));
    env.config(&agent_config(&env, "network = true"));
    let out = env.run(&["run", "--json", "--no-learn", "ask the scout"]);
    assert!(out.status.success(), "{}", text(&out));
    let end = env.journal().into_iter().find(|r| r.ty == "tool_end" && r.payload["name"] == "delegate").unwrap();
    assert_eq!(end.payload["ok"], true, "{}", end.payload);
}

#[test]
fn an_agent_that_ignores_stdin_closing_is_stopped_once_its_turn_is_done() {
    // codex-acp keeps running after its answer. Its work was done, so that is
    // neither a timeout nor a failure, and the parent must not wait for it.
    let started = std::time::Instant::now();
    let (env, out) = ext_run("acp-ext-linger", "timeout = 60", &["--linger"]);
    assert!(out.status.success(), "{}", text(&out));
    let end = env.journal().into_iter().find(|r| r.ty == "tool_end" && r.payload["name"] == "delegate").unwrap();
    assert_eq!(end.payload["ok"], true, "{}", end.payload);
    assert!(started.elapsed().as_secs() < budget(30), "waited for the lingering agent");
}

#[test]
fn an_agents_env_reaches_the_child_and_construct_owned_names_are_refused() {
    let (env, out) = ext_run("acp-ext-env", "env = { GREETING = \"hello there\" }", &["--report-env", "GREETING"]);
    assert!(out.status.success(), "{}", text(&out));
    let back = last_tool_result(env.requests().last().unwrap());
    assert!(back.contains("GREETING=hello there"), "{back}");

    let (_, out) = ext_run("acp-ext-env-bad", "env = { CONSTRUCT_HOME = \"/elsewhere\" }", &[]);
    assert!(!out.status.success());
    assert!(text(&out).contains("is not a variable a definition may set"), "{}", text(&out));
}
