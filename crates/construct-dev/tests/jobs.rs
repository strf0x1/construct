//! Background jobs end to end (ADR 0026, #34): the real binary runs a
//! background command, the turn ends, and the completion comes back as a turn
//! of its own. Steps are served in request order, so the order is
//! deterministic: the wake turn is the request after the one that ended the
//! first turn.

use crate::common::*;
use serde_json::{Value, json};

fn shell_bg(command: &str) -> Value {
    json!({ "call": "shell", "args": { "command": command, "background": true } })
}

fn rows(env: &Env, ty: &str) -> Vec<Row> {
    env.journal().into_iter().filter(|r| r.ty == ty).collect()
}

/// The text of the last message in a request.
fn last_message(req: &Value) -> (String, String) {
    let m = req["messages"].as_array().unwrap().last().unwrap().clone();
    (m["role"].as_str().unwrap().into(), m["content"].as_str().unwrap_or("").into())
}

#[test]
fn a_background_command_returns_a_handle_and_its_end_starts_a_turn() {
    let env = Env::new(
        "jobs-wake",
        json!({ "steps": [
            shell_bg("sleep 1 && echo done"),
            { "text": "started; waiting" },
            { "text": "it said done" }
        ] }),
    );
    let out = env.run(&["run", "--json", "--no-learn", "watch it"]);
    assert!(out.status.success(), "{}", text(&out));

    let reqs = env.requests();
    assert_eq!(reqs.len(), 3, "{}", text(&out));
    // The handle came back at once, and says it will not need polling.
    let handle = last_tool_result(&reqs[1]);
    assert!(handle.contains("started job j1") && handle.contains("don't poll"), "{handle}");
    // The turn ended; the next request is the completion, tagged, with the tail.
    let (role, body) = last_message(&reqs[2]);
    assert_eq!(role, "user");
    assert!(body.contains(r#"<job id="j1" kind="shell" status="completed" exit="0""#), "{body}");
    assert!(body.contains("done\n</job>"), "{body}");
    // The job tool joined the request once there was a job to look at.
    assert!(!tool_names(&reqs[0]).contains(&"job".to_string()));
    assert!(tool_names(&reqs[2]).contains(&"job".to_string()));

    // The journal: start, end, then a delivery that names it and carries the
    // message, between two turns; and the only user row is the user's.
    let types: Vec<String> = env.journal().into_iter().map(|r| r.ty).collect();
    let at = |t: &str| types.iter().position(|x| x == t).unwrap();
    assert!(at("job_start") < at("job_end") && at("job_end") < at("jobs_delivered"), "{types:?}");
    assert_eq!(types.iter().filter(|t| *t == "turn_start").count(), 2, "{types:?}");
    assert_eq!(types.iter().filter(|t| *t == "user").count(), 1, "a completion is never a user row: {types:?}");
    let delivered = &rows(&env, "jobs_delivered")[0].payload;
    assert_eq!((delivered["ids"].clone(), delivered["via"].as_str()), (json!(["j1"]), Some("turn")));
    let end = &rows(&env, "job_end")[0].payload;
    assert_eq!((end["status"].as_str(), end["exit"].as_i64()), (Some("completed"), Some(0)));
    assert!(end["tail"].as_str().unwrap().contains("done"));
    // `--json` shows the same events.
    let printed = String::from_utf8_lossy(&out.stdout);
    assert!(printed.contains(r#""event":"job_end""#) && printed.contains(r#""event":"jobs_delivered""#));
}

#[test]
fn a_cancelled_job_is_journaled_and_never_wakes() {
    let env = Env::new(
        "jobs-cancel",
        json!({ "steps": [
            shell_bg("sleep 300"),
            { "call": "job", "args": { "action": "cancel", "id": "j1" } },
            { "text": "cancelled it" }
        ] }),
    );
    let out = env.run(&["run", "--no-learn", "start and stop"]);
    assert!(out.status.success(), "{}", text(&out));

    // Nothing woke the agent: three requests, one turn, no delivery.
    assert_eq!(env.requests().len(), 3, "{}", text(&out));
    assert_eq!(rows(&env, "turn_start").len(), 1);
    assert!(rows(&env, "jobs_delivered").is_empty());
    // And the ending is on record.
    let end = &rows(&env, "job_end");
    assert_eq!(end.len(), 1);
    assert_eq!(end[0].payload["status"], "cancelled");
}

#[test]
fn a_command_past_its_time_limit_ends_as_a_timeout_and_wakes() {
    let env = Env::new(
        "jobs-timeout",
        json!({ "steps": [shell_bg("sleep 300"), { "text": "ok" }, { "text": "it timed out" }] }),
    );
    env.config("[tools.shell]\nbackground_timeout = 1\n");
    let out = env.run(&["run", "--no-learn", "wait"]);
    assert!(out.status.success(), "{}", text(&out));
    let reqs = env.requests();
    assert_eq!(reqs.len(), 3, "{}", text(&out));
    assert!(last_message(&reqs[2]).1.contains(r#"status="timeout""#));
}

#[test]
fn reading_a_result_with_check_means_it_does_not_wake_again() {
    let env = Env::new(
        "jobs-check",
        json!({ "steps": [
            shell_bg("echo quick"),
            { "call": "shell", "args": { "command": "sleep 1" } },
            { "call": "job", "args": { "action": "check", "id": "j1" } },
            { "text": "read it" }
        ] }),
    );
    let out = env.run(&["run", "--no-learn", "go"]);
    assert!(out.status.success(), "{}", text(&out));
    let reqs = env.requests();
    assert_eq!(reqs.len(), 4, "no wake turn after the model read the result: {}", text(&out));
    let seen = last_tool_result(&reqs[3]);
    assert!(seen.contains("completed") && seen.contains("quick"), "{seen}");
    let delivered = rows(&env, "jobs_delivered");
    assert_eq!(delivered.len(), 1);
    assert_eq!(delivered[0].payload["via"], "check");
    assert!(delivered[0].payload.get("message").is_none());
}

#[test]
fn jobs_past_the_limit_are_refused() {
    let env = Env::new(
        "jobs-limit",
        json!({ "steps": [
            shell_bg("sleep 300"),
            shell_bg("sleep 300"),
            { "call": "job", "args": { "action": "cancel", "id": "j1" } },
            { "text": "done" }
        ] }),
    );
    env.config("[jobs]\nlimit = 1\n");
    let out = env.run(&["run", "--no-learn", "two"]);
    assert!(out.status.success(), "{}", text(&out));
    let refused = last_tool_result(&env.requests()[2]);
    assert!(refused.contains("the limit"), "{refused}");
    assert_eq!(rows(&env, "job_start").len(), 1);
}

#[test]
fn ctrl_c_during_a_run_turn_interrupts_it_and_journals_the_job_as_cancelled() {
    let env = Env::new(
        "jobs-sigint",
        json!({ "steps": [
            shell_bg("sleep 300"),
            { "call": "shell", "args": { "command": "touch started && sleep 300" } },
            { "text": "never" }
        ] }),
    );
    let mut c = std::process::Command::new(construct());
    c.args(["run", "--no-learn", "start and wait"]).current_dir(&env.work);
    for (k, v) in env.env() {
        if v.is_empty() {
            c.env_remove(k);
        } else {
            c.env(k, v);
        }
    }
    let mut run = c.stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn().unwrap();
    let marker = env.work.join("started");
    let end = std::time::Instant::now() + std::time::Duration::from_millis(budget(15_000));
    while !marker.exists() && std::time::Instant::now() < end {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(marker.exists(), "the foreground command never started");
    let sent = std::process::Command::new("kill").args(["-INT", &run.id().to_string()]).status().unwrap();
    assert!(sent.success());
    // It ends on its own, well inside the sleeps it was interrupted from.
    let waited = std::time::Instant::now();
    let status = run.wait().unwrap();
    assert!(waited.elapsed().as_secs() < budget(30), "construct kept running after Ctrl-C");
    assert!(status.success(), "{status:?}");

    let end = rows(&env, "job_end");
    assert_eq!((end.len(), end[0].payload["status"].clone()), (1, json!("cancelled")));
    let turn_end = rows(&env, "turn_end");
    assert_eq!(turn_end[0].payload["interrupted"], true);
    assert!(rows(&env, "jobs_delivered").is_empty());
    assert_eq!(env.requests().len(), 2, "nothing asked the model again");
}

/// A job's end arrives whenever it does; one that lands while a reply streams
/// must not split the reply on screen. The stream is slowed (`chunk_ms`) so
/// the one-second job ends after its first words and before its last.
#[test]
fn a_job_ending_mid_reply_leaves_the_reply_whole_and_its_notice_after_it() {
    let reply = "the job runs while this reply streams in one word at a time and stays one line";
    let env = Env::new(
        "jobs-mid-reply",
        json!({ "chunk_ms": 250, "steps": [shell_bg("sleep 1 && echo early"), { "text": reply }, { "text": "it said early" }] }),
    );
    env.config("[router]\nmode = 'local'\n[learn]\nenabled = false\n");
    let mut d = construct_dev::drive::Driver::spawn(
        &[construct().display().to_string()],
        &env.env(),
        Some(env.work.to_str().unwrap()),
        110,
        34,
    )
    .unwrap();
    let script = format!(
        "wait \"ask construct\" 5000\ntype watch it\nkey enter\nwait \"{reply}\" 10000\nwait \"it said early\" 8000\n"
    );
    let mut log = vec![];
    if let Err(e) = construct_dev::drive::run_script(&mut d, &script, &mut log) {
        panic!("{e:#}\n{}", String::from_utf8_lossy(&log));
    }
    // It really ended mid-reply: before the turn that streamed the reply.
    let types: Vec<String> = env.journal().into_iter().map(|r| r.ty).collect();
    let at = |t: &str| types.iter().position(|x| x == t).unwrap();
    assert!(at("job_end") < at("turn_end"), "the job did not end mid-reply: {types:?}");
    let screen = d.text();
    let line = |s: &str| screen.lines().position(|l| l.contains(s)).unwrap_or_else(|| panic!("no {s:?} in\n{screen}"));
    assert!(line("job j1 completed") > line(reply), "{screen}");
}
