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

/// Rows of one type in the first (parent) session: a child's own turns are
/// journaled under its session and are not the parent's.
fn parent_rows(env: &Env, ty: &str) -> Vec<Row> {
    let parent = env.sessions().remove(0);
    rows(env, ty).into_iter().filter(|r| r.session == parent).collect()
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

// Background delegates. The parent and its children share one scripted
// endpoint, which serves steps in request order, so each parent batch ends in
// a foreground `sleep`: the children's first requests land inside it, and
// their later ones (after their own longer sleeps) land after the parent's
// next request.

fn scout_config(extra: &str) -> String {
    format!(
        "[agents.scout]\ndescription = \"waits and reports\"\nprompt = \"You are a scout.\"\ntools = [\"shell\"]\n{extra}\n"
    )
}

fn delegate_bg(task: &str) -> Value {
    json!({ "call": "delegate", "args": { "agent": "scout", "task": task, "background": true } })
}

fn sleep(secs: u32) -> Value {
    json!({ "call": "shell", "args": { "command": format!("sleep {secs}") } })
}

fn tool_results(req: &Value) -> Vec<String> {
    req["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["role"] == "tool")
        .map(|m| m["content"].as_str().unwrap_or("").to_string())
        .collect()
}

#[test]
fn a_background_delegate_returns_a_handle_and_its_summary_starts_a_turn() {
    let env = Env::new(
        "jobs-delegate",
        json!({ "steps": [
            { "calls": [delegate_bg("wait, then report"), sleep(1)] },
            sleep(2),
            { "text": "sent the scout off" },
            { "text": "scout-report: all clear" },
            { "text": "the scout says all clear" }
        ] }),
    );
    env.config(&scout_config(""));
    let out = env.run(&["run", "--json", "--no-learn", "send the scout"]);
    assert!(out.status.success(), "{}", text(&out));

    let reqs = env.requests();
    assert_eq!(reqs.len(), 5, "{}", text(&out));
    // The child asked for its own prompt and tools, while the parent went on.
    assert_eq!(tool_names(&reqs[1]), vec!["shell"]);
    assert_eq!(reqs[1]["messages"][0]["content"], "You are a scout.");
    let handle = tool_results(&reqs[2]).remove(0);
    assert!(handle.contains("started job j1") && handle.contains("don't poll"), "{handle}");
    // The completion is a tagged message that carries the summary and the pointer.
    let (role, body) = last_message(&reqs[4]);
    assert_eq!(role, "user");
    assert!(body.contains(r#"<job id="j1" kind="delegate" status="completed""#), "{body}");
    assert!(body.contains("scout-report: all clear") && body.contains("[full transcript: journal session "), "{body}");

    // The journal: the parent's session holds the job rows, in order, and
    // exactly one user row; the child's own events are under its session.
    let sessions = env.sessions();
    assert_eq!(sessions.len(), 2, "{sessions:?}");
    let (parent, child) = (&sessions[0], &sessions[1]);
    let of = |s: &String| env.journal().into_iter().filter(|r| &r.session == s).collect::<Vec<_>>();
    let types: Vec<String> = of(parent).into_iter().map(|r| r.ty).collect();
    let at = |t: &str| types.iter().position(|x| x == t).unwrap();
    assert!(at("job_start") < at("job_end") && at("job_end") < at("jobs_delivered"), "{types:?}");
    assert_eq!(types.iter().filter(|t| *t == "turn_start").count(), 2, "{types:?}");
    assert_eq!(types.iter().filter(|t| *t == "user").count(), 1, "{types:?}");
    let child_rows = of(child);
    assert!(child_rows.iter().any(|r| r.ty == "tool_result"), "the child's work is in the record");
    let start = &rows(&env, "job_start")[0].payload;
    assert_eq!(start["kind"], "delegate");
    assert_eq!(child_rows[0].payload["parent_call_id"], start["call_id"]);
    let end = &rows(&env, "job_end")[0].payload;
    assert_eq!(end["status"], "completed");
    assert!(end["tail"].as_str().unwrap().contains(&format!("journal session {child}")));
    // The call that started it ended at once, with the handle as its data.
    let ended = env.journal().into_iter().find(|r| r.ty == "tool_end" && r.payload["name"] == "delegate").unwrap();
    assert_eq!(ended.payload["data"]["job"], "j1");
}

#[test]
fn two_background_children_finish_together_and_a_third_is_refused_at_the_limit() {
    let env = Env::new(
        "jobs-delegate-two",
        json!({ "steps": [
            { "calls": [delegate_bg("one"), delegate_bg("two"), delegate_bg("three"), sleep(1)] },
            sleep(2),
            sleep(2),
            // The parent is still in a turn when both children end, so their
            // endings are delivered together at its boundary.
            sleep(3),
            { "text": "alpha" },
            { "text": "beta" },
            { "text": "waiting for the scouts" },
            { "text": "both reported" }
        ] }),
    );
    env.config(&format!("[jobs]\nlimit = 2\n{}", scout_config("max_background = 5")));
    let out = env.run(&["run", "--no-learn", "send three scouts"]);
    assert!(out.status.success(), "{}", text(&out));

    let reqs = env.requests();
    assert_eq!(reqs.len(), 8, "{}", text(&out));
    // The third was refused with the job limit, the first two got handles.
    let results = tool_results(&reqs[3]);
    assert!(results[0].contains("started job j1") && results[1].contains("started job j2"), "{results:?}");
    assert!(results[2].contains("the limit"), "{results:?}");
    let refused: Vec<_> = env
        .journal()
        .into_iter()
        .filter(|r| r.ty == "tool_end" && r.payload["name"] == "delegate" && r.payload["code"] == "job_limit")
        .collect();
    assert_eq!(refused.len(), 1);
    assert_eq!(rows(&env, "job_start").len(), 2);
    // One wake turn, with both completions in one message.
    let (_, body) = last_message(&reqs[7]);
    assert!(body.contains(r#"<job id="j1""#) && body.contains(r#"<job id="j2""#), "{body}");
    assert!(body.contains("alpha") && body.contains("beta"), "{body}");
    assert_eq!(parent_rows(&env, "turn_start").len(), 2);
    let delivered = &rows(&env, "jobs_delivered");
    assert_eq!((delivered.len(), delivered[0].payload["ids"].clone()), (1, json!(["j1", "j2"])));
    // Each child is a session of its own, with the parent's session as its parent.
    assert_eq!(env.sessions().len(), 3);
}

#[test]
fn an_agent_past_its_background_cap_is_refused_and_a_cancelled_child_wakes_nobody() {
    let env = Env::new(
        "jobs-delegate-cap",
        json!({ "steps": [
            { "calls": [delegate_bg("one"), delegate_bg("two"), sleep(1)] },
            sleep(300),
            { "call": "job", "args": { "action": "cancel", "id": "j1" } },
            { "text": "cancelled it" }
        ] }),
    );
    env.config(&scout_config("max_background = 1"));
    let out = env.run(&["run", "--no-learn", "two scouts"]);
    assert!(out.status.success(), "{}", text(&out));

    let reqs = env.requests();
    assert_eq!(reqs.len(), 4, "no wake turn for a cancelled child: {}", text(&out));
    let results = tool_results(&reqs[2]);
    assert!(results[0].contains("started job j1"), "{results:?}");
    assert!(results[1].contains("scout are already going") && results[1].contains("its limit"), "{results:?}");
    let end = &rows(&env, "job_end");
    assert_eq!((end.len(), end[0].payload["status"].clone()), (1, json!("cancelled")));
    assert!(rows(&env, "jobs_delivered").is_empty());
    assert_eq!(parent_rows(&env, "turn_start").len(), 1);
}

#[test]
fn an_agent_with_no_background_places_refuses_a_background_call() {
    let env = Env::new(
        "jobs-delegate-zero",
        json!({ "steps": [{ "call": "delegate", "args": { "agent": "scout", "task": "x", "background": true } }, { "text": "ok" }] }),
    );
    env.config(&scout_config("max_background = 0"));
    let out = env.run(&["run", "--no-learn", "try"]);
    assert!(out.status.success(), "{}", text(&out));
    let refused = last_tool_result(&env.requests()[1]);
    assert!(refused.contains("max_background = 0"), "{refused}");
    assert!(rows(&env, "job_start").is_empty());
}

#[test]
fn a_child_past_its_time_limit_ends_the_job_as_a_timeout_and_wakes() {
    let env = Env::new(
        "jobs-delegate-timeout",
        json!({ "steps": [
            { "calls": [delegate_bg("wait forever"), sleep(1)] },
            sleep(300),
            { "text": "sent it" },
            { "text": "it timed out" }
        ] }),
    );
    env.config(&scout_config("timeout = 1"));
    let out = env.run(&["run", "--no-learn", "send the scout"]);
    assert!(out.status.success(), "{}", text(&out));
    let end = &rows(&env, "job_end")[0].payload;
    assert_eq!(end["status"], "timeout");
    assert!(end["tail"].as_str().unwrap().contains("timed out after 1s"), "{end}");
    let reqs = env.requests();
    assert_eq!(reqs.len(), 4, "{}", text(&out));
    assert!(last_message(&reqs[3]).1.contains(r#"status="timeout""#));
}
