//! Deck's gateway contract: denial classification, boundary guidance, and the
//! late-decision push. The transport is the same scripted server as the other
//! e2e tests; the boundary endpoint is a test-local HTTP server.

use crate::acp::Client;
use crate::common::*;
use serde_json::json;
use std::time::{Duration, Instant};

fn deck(code: &str, extra: &str) -> String {
    format!(r#"{{"error":{{"message":"policy refused the request","type":"{code}","code":"{code}"}}{extra}}}"#)
}

#[test]
fn a_credential_denial_fails_after_one_attempt() {
    let env = Env::new(
        "deck-credential",
        json!({ "steps": [
            { "status": 503, "body": deck("deck_credential_missing", "") },
            { "text": "this must not be reached" }
        ] }),
    );
    let out = env.run(&["run", "--no-learn", "hi"]);
    assert!(!out.status.success(), "a missing credential is policy, not an outage: {}", text(&out));
    assert!(text(&out).contains("deck_credential_missing"), "{}", text(&out));
    assert_eq!(env.requests().len(), 1, "a 503 deck_credential_missing must not be retried");
}

#[test]
fn a_boundary_denial_names_the_approval_and_is_not_retried() {
    let env = Env::new(
        "deck-denied",
        json!({ "steps": [
            { "status": 403, "body": deck("deck_boundary_denied", r#","approval_id":"a1f3","scope":"model:openrouter/free""#) },
            { "text": "this must not be reached" }
        ] }),
    );
    let out = env.run(&["run", "--no-learn", "hi"]);
    assert!(!out.status.success(), "{}", text(&out));
    let shown = text(&out);
    assert!(shown.contains("deck_boundary_denied"), "{shown}");
    assert!(shown.contains("a1f3"), "the approval id must reach the agent: {shown}");
    assert!(shown.contains("model:openrouter/free"), "the requested scope must reach the agent: {shown}");
    assert_eq!(env.requests().len(), 1);
}

#[test]
fn a_provider_server_error_still_retries() {
    let env = Env::new(
        "deck-provider-500",
        json!({ "steps": [
            { "status": 500, "body": r#"{"error":{"message":"upstream exploded","type":"server_error"}}"# },
            { "text": "recovered" }
        ] }),
    );
    let out = env.run(&["run", "--no-learn", "hi"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(text(&out).contains("recovered"));
    assert_eq!(env.requests().len(), 2, "a real 500 keeps the retry path");
}

#[test]
fn deck_upstream_failed_keeps_retrying() {
    let env = Env::new(
        "deck-upstream-502",
        json!({ "steps": [
            { "status": 502, "body": deck("deck_upstream_failed", "") },
            { "text": "recovered" }
        ] }),
    );
    let out = env.run(&["run", "--no-learn", "hi"]);
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(env.requests().len(), 2, "deck_upstream_failed is the one retryable Deck code");
}

#[test]
fn a_non_deck_body_falls_back_to_the_status_and_text() {
    let env = Env::new("deck-fallback", json!({ "steps": [{ "status": 403, "body": "proxy said no" }] }));
    let out = env.run(&["run", "--no-learn", "hi"]);
    assert!(!out.status.success());
    let shown = text(&out);
    assert!(shown.contains("model endpoint returned 403"), "{shown}");
    assert!(shown.contains("proxy said no"), "{shown}");
    assert_eq!(env.requests().len(), 1);
}

fn acp(env: &Env, extra: &[(&str, &str)]) -> (Client, String) {
    let mut c = Client::start_with(env, extra);
    c.call("initialize", json!({ "protocolVersion": 1, "clientCapabilities": {} }));
    let (new, _) = c.call("session/new", json!({ "cwd": env.work.display().to_string(), "mcpServers": [] }));
    (c, new["result"]["sessionId"].as_str().unwrap().to_string())
}

#[test]
fn a_blocked_tool_result_carries_the_boundary_note() {
    let server = boundary::Server::start();
    server.arm("a1f3", "index.crates.io:443", "pending", None);
    let env = Env::new(
        "deck-note",
        json!({ "steps": [
            { "call": "shell", "args": { "command": "true" } },
            { "text": "done" }
        ] }),
    );
    let out = env.run_with(&[("DECK_BOUNDARY_URL", server.url().as_str())], &["run", "--no-learn", "build it"]);
    assert!(out.status.success(), "{}", text(&out));
    let reqs = env.requests();
    assert_eq!(reqs.len(), 2);
    let tool = last_tool_result(&reqs[1]);
    assert!(tool.contains("[deck boundary]"), "{tool}");
    assert!(tool.contains("index.crates.io:443"), "{tool}");
    assert!(tool.contains("a1f3"), "{tool}");
    assert!(tool.contains("do not look for another route"), "{tool}");
    // The journal's tool_end is what a scorer reads; it must agree with the model.
    let rows = env.journal();
    let end = rows.iter().find(|r| r.ty == "tool_end").expect("a tool_end row");
    assert!(end.payload["output"].as_str().unwrap().contains("a1f3"));
}

#[test]
fn malformed_timed_event_does_not_disable_a_later_corrected_record() {
    let server = boundary::Server::start();
    server.pending("a1f3", "index.crates.io:443");
    server.event_responses([
        json!([]),
        json!([{
            "seq": 1, "approval_id": "a1f3", "destination": "index.crates.io:443",
            "state": "allowed", "scope": "timed"
        }]),
    ]);
    server.resolve_with_deadline("a1f3", "allowed", "timed", 1_800_000_300_000);
    let env = Env::new(
        "deck-malformed-recovery",
        json!({ "steps": [
            { "call": "shell", "args": { "command": "true" } },
            { "call": "shell", "args": { "command": "true" } },
            { "text": "done" }
        ] }),
    );
    let out = env.run_with(&[("DECK_BOUNDARY_URL", server.url().as_str())], &["run", "--no-learn", "recover"]);
    assert!(out.status.success(), "{}", text(&out));
    let requests = env.requests();
    assert!(!last_tool_result(&requests[1]).contains("[deck boundary]"));
    let corrected = last_tool_result(&requests[2]);
    assert!(corrected.contains("a1f3"), "{corrected}");
    assert!(corrected.contains("expires_at_ms=1800000300000 (Unix ms)"), "{corrected}");
}

#[test]
fn malformed_status_does_not_disable_a_later_corrected_response() {
    let server = boundary::Server::start();
    server.pending("a1f3", "index.crates.io:443");
    server.resolve_with_deadline("a1f3", "allowed", "timed", 1_800_000_300_000);
    server.approval_responses(
        "a1f3",
        [json!({
            "approval_id": "a1f3", "destination": "index.crates.io:443",
            "state": "allowed", "scope": "timed"
        })],
    );
    let env = Env::new(
        "deck-malformed-status-recovery",
        json!({ "steps": [
            { "call": "boundary_status", "args": { "id": "a1f3" } },
            { "call": "boundary_status", "args": { "id": "a1f3" } },
            { "text": "done" }
        ] }),
    );
    let out = env.run_with(&[("DECK_BOUNDARY_URL", server.url().as_str())], &["run", "--no-learn", "recover"]);
    assert!(out.status.success(), "{}", text(&out));
    let requests = env.requests();
    assert!(last_tool_result(&requests[1]).contains("timed boundary observation is missing expires_at_ms"));
    let corrected = last_tool_result(&requests[2]);
    assert!(corrected.contains("expires_at_ms=1800000300000 (Unix ms)"), "{corrected}");
}

#[test]
fn boundary_wait_returns_allowed_with_its_scope() {
    let server = boundary::Server::start();
    server.pending("a1f3", "index.crates.io:443");
    server.resolve_after("a1f3", "allowed", Some("task"), Duration::from_millis(150));
    let env = Env::new(
        "deck-wait",
        json!({ "steps": [
            { "call": "boundary_wait", "args": { "id": "a1f3", "timeout_s": 5 } },
            { "text": "done" }
        ] }),
    );
    let started = Instant::now();
    let out = env.run_with(&[("DECK_BOUNDARY_URL", server.url().as_str())], &["run", "--no-learn", "wait for it"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(started.elapsed() >= Duration::from_millis(140), "{:?}", started.elapsed());
    let tool = last_tool_result(&env.requests()[1]);
    assert!(tool.contains("allowed") && tool.contains("scope task"), "{tool}");
    let asked = server.requests();
    assert!(asked.iter().any(|r| r.starts_with("GET /approvals/a1f3") && r.contains("wait=5")), "{asked:?}");
}

#[test]
fn boundary_status_preserves_the_hosts_absolute_timed_deadline() {
    const DEADLINE: u64 = 1_800_000_300_000;
    let server = boundary::Server::start();
    server.pending("a1f3", "index.crates.io:443");
    server.resolve_with_deadline("a1f3", "allowed", "timed", DEADLINE);
    let env = Env::new(
        "deck-timed-status",
        json!({ "steps": [
            { "call": "boundary_status", "args": { "id": "a1f3" } },
            { "text": "done" }
        ] }),
    );
    let out = env.run_with(&[("DECK_BOUNDARY_URL", server.url().as_str())], &["run", "--no-learn", "check it"]);
    assert!(out.status.success(), "{}", text(&out));
    let tool = last_tool_result(&env.requests()[1]);
    assert!(tool.contains("scope timed"), "{tool}");
    assert!(tool.contains("expires_at_ms=1800000300000 (Unix ms)"), "{tool}");
    let end = env.journal().into_iter().find(|row| row.ty == "tool_end").unwrap();
    assert_eq!(end.payload["data"]["scope"], "timed", "{}", end.payload);
    assert_eq!(end.payload["data"]["expires_at_ms"], DEADLINE, "{}", end.payload);
}

#[test]
fn boundary_status_reports_pending_and_withdraw_removes_it() {
    let server = boundary::Server::start();
    server.pending("a1f3", "index.crates.io:443");
    let env = Env::new(
        "deck-status",
        json!({ "steps": [
            { "call": "boundary_status", "args": { "id": "a1f3" } },
            { "call": "boundary_withdraw", "args": { "id": "a1f3" } },
            { "text": "done" }
        ] }),
    );
    let out = env.run_with(&[("DECK_BOUNDARY_URL", server.url().as_str())], &["run", "--no-learn", "check it"]);
    assert!(out.status.success(), "{}", text(&out));
    let reqs = env.requests();
    let status = last_tool_result(&reqs[1]);
    assert!(status.contains("pending with the user"), "{status}");
    let withdrew = last_tool_result(&reqs[2]);
    assert!(withdrew.contains("withdrew approval a1f3"), "{withdrew}");
    assert_eq!(server.approval("a1f3").unwrap().state, "withdrawn");
    assert!(
        server.requests().iter().any(|r| r.starts_with("POST /approvals/a1f3/withdraw")),
        "{:?}",
        server.requests()
    );
}

#[test]
fn a_cancelled_wait_ends_the_turn() {
    let server = boundary::Server::start();
    server.pending("a1f3", "index.crates.io:443");
    let env = Env::new(
        "deck-cancel",
        json!({ "steps": [
            { "call": "boundary_wait", "args": { "id": "a1f3", "timeout_s": 30 } },
            { "text": "not reached" }
        ] }),
    );
    let (mut c, sid) = acp(&env, &[("DECK_BOUNDARY_URL", server.url().as_str())]);
    let id = c.request(
        "session/prompt",
        json!({
            "sessionId": sid,
            "prompt": [{ "type": "text", "text": "wait" }]
        }),
    );
    loop {
        let v = c.read();
        if v["method"] == "session/update" && v["params"]["update"]["sessionUpdate"] == "tool_call" {
            break;
        }
        assert!(v["id"] != json!(id), "turn ended before boundary_wait started: {v}");
    }
    let started = Instant::now();
    c.notify("session/cancel", json!({ "sessionId": sid }));
    let (done, _) = c.await_response(id);
    assert_eq!(done["result"]["stopReason"], "cancelled", "{done}");
    assert!(started.elapsed() < Duration::from_millis(budget(3_000)), "{:?}", started.elapsed());
    c.finish();
}

#[test]
fn initialize_advertises_the_deck_boundary_capability() {
    let env = Env::new("deck-cap", json!({ "steps": [{ "text": "ok" }] }));
    let server = boundary::Server::start();
    let mut c = Client::start_with(&env, &[("DECK_BOUNDARY_URL", server.url().as_str())]);
    let (init, _) = c.call("initialize", json!({ "protocolVersion": 1, "clientCapabilities": {} }));
    assert_eq!(init["result"]["agentCapabilities"]["_meta"]["deck"]["boundary"], json!(true), "{init}");
    assert_eq!(
        init["result"]["agentCapabilities"]["_meta"]["com.sprawlcommons/deck"]["boundary"],
        json!(true),
        "{init}"
    );
    c.finish();
}

#[test]
fn a_late_decision_reaches_the_next_request_exactly_once() {
    let env = Env::new("deck-push", json!({ "steps": [{ "text": "one" }, { "text": "two" }] }));
    env.config("[router]\nmode = \"local\"\n");
    let server = boundary::Server::start();
    let (mut c, sid) = acp(&env, &[("DECK_BOUNDARY_URL", server.url().as_str())]);
    let (first, _) = c.call(
        "session/prompt",
        json!({
            "sessionId": sid,
            "prompt": [{ "type": "text", "text": "first" }]
        }),
    );
    assert_eq!(first["result"]["stopReason"], "end_turn");
    c.notify(
        "_deck/boundary",
        json!({ "approval_id": "a1f3", "destination": "index.crates.io:443", "outcome": "allowed", "scope": "task" }),
    );
    c.notify(
        "_deck/boundary",
        json!({ "approval_id": "a1f3", "destination": "index.crates.io:443", "outcome": "allowed", "scope": "task" }),
    );
    let (second, _) = c.call(
        "session/prompt",
        json!({
            "sessionId": sid,
            "prompt": [{ "type": "text", "text": "second" }]
        }),
    );
    assert_eq!(second["result"]["stopReason"], "end_turn");
    let reqs = env.requests();
    let text = reqs.last().unwrap().to_string();
    assert_eq!(text.matches("[deck boundary]").count(), 1, "{text}");
    assert!(text.contains("a1f3") && text.contains("allowed"), "{text}");
    c.finish();
}

#[test]
fn a_past_timed_deadline_is_preserved_in_a_late_notice_without_replaying_work() {
    const DEADLINE: u64 = 42;
    let env = Env::new("deck-push-timed", json!({ "steps": [{ "text": "one" }, { "text": "two" }] }));
    env.config("[router]\nmode = \"local\"\n");
    let server = boundary::Server::start();
    let (mut c, sid) = acp(&env, &[("DECK_BOUNDARY_URL", server.url().as_str())]);
    c.call("session/prompt", json!({ "sessionId": sid, "prompt": [{ "type": "text", "text": "first" }] }));
    c.notify(
        "_com.sprawlcommons/deck/boundary",
        json!({
            "approval_id": "a1f3", "destination": "index.crates.io:443", "outcome": "allowed",
            "scope": "timed", "expires_at_ms": DEADLINE
        }),
    );
    c.notify(
        "_deck/boundary",
        json!({
            "approval_id": "a1f3", "destination": "index.crates.io:443", "outcome": "allowed",
            "scope": "timed", "expires_at_ms": DEADLINE
        }),
    );
    c.call("session/prompt", json!({ "sessionId": sid, "prompt": [{ "type": "text", "text": "second" }] }));
    let requests = env.requests();
    let text = requests.last().unwrap().to_string();
    assert_eq!(text.matches("[deck boundary]").count(), 1, "{text}");
    assert!(text.contains("expires_at_ms=42 (Unix ms)"), "{text}");
    assert_eq!(requests.len(), 2, "a notice must not create another model/tool exchange");
    c.finish();
}

#[test]
fn a_late_decision_on_the_reverse_dns_method_is_accepted() {
    let env = Env::new("deck-push-new", json!({ "steps": [{ "text": "one" }, { "text": "two" }] }));
    env.config("[router]\nmode = \"local\"\n");
    let server = boundary::Server::start();
    let (mut c, sid) = acp(&env, &[("DECK_BOUNDARY_URL", server.url().as_str())]);
    let (first, _) =
        c.call("session/prompt", json!({ "sessionId": sid, "prompt": [{ "type": "text", "text": "first" }] }));
    assert_eq!(first["result"]["stopReason"], "end_turn");
    c.notify(
        "_com.sprawlcommons/deck/boundary",
        json!({ "approval_id": "a1f3", "destination": "index.crates.io:443", "outcome": "allowed", "scope": "task" }),
    );
    let (second, _) =
        c.call("session/prompt", json!({ "sessionId": sid, "prompt": [{ "type": "text", "text": "second" }] }));
    assert_eq!(second["result"]["stopReason"], "end_turn");
    let text = env.requests().last().unwrap().to_string();
    assert!(text.contains("[deck boundary]") && text.contains("a1f3"), "{text}");
    c.finish();
}

#[test]
fn an_id_already_seen_through_status_is_not_pushed_back() {
    let server = boundary::Server::start();
    server.pending("a1f3", "index.crates.io:443");
    server.resolve("a1f3", "allowed", Some("task"));
    let env = Env::new(
        "deck-seen",
        json!({ "steps": [
            { "call": "boundary_status", "args": { "id": "a1f3" } },
            { "text": "one" },
            { "text": "two" }
        ] }),
    );
    env.config("[router]\nmode = \"local\"\n");
    let (mut c, sid) = acp(&env, &[("DECK_BOUNDARY_URL", server.url().as_str())]);
    c.call(
        "session/prompt",
        json!({
            "sessionId": sid,
            "prompt": [{ "type": "text", "text": "first" }]
        }),
    );
    c.notify(
        "_deck/boundary",
        json!({ "approval_id": "a1f3", "destination": "index.crates.io:443", "outcome": "allowed", "scope": "task" }),
    );
    let (second, _) = c.call(
        "session/prompt",
        json!({
            "sessionId": sid,
            "prompt": [{ "type": "text", "text": "second" }]
        }),
    );
    assert_eq!(second["result"]["stopReason"], "end_turn");
    let text = env.requests().last().unwrap().to_string();
    assert!(!text.contains("[deck boundary]"), "{text}");
    c.finish();
}

#[test]
fn a_late_decision_lands_mid_turn_before_the_next_request() {
    let env = Env::new(
        "deck-mid",
        json!({ "steps": [
            { "call": "shell", "args": { "command": "true" } },
            { "text": "done" }
        ] }),
    );
    env.config("[router]\nmode = \"local\"\n");
    let server = boundary::Server::start();
    let (mut c, sid) = acp(&env, &[("DECK_BOUNDARY_URL", server.url().as_str())]);
    let id = c.request(
        "session/prompt",
        json!({
            "sessionId": sid,
            "prompt": [{ "type": "text", "text": "run it" }]
        }),
    );
    loop {
        let v = c.read();
        if v["method"] == "session/update" && v["params"]["update"]["sessionUpdate"] == "tool_call" {
            break;
        }
        assert!(v["id"] != json!(id), "turn ended before the call: {v}");
    }
    c.notify(
        "_deck/boundary",
        json!({ "approval_id": "a1f3", "destination": "index.crates.io:443", "outcome": "allowed", "scope": "task" }),
    );
    let (done, _) = c.await_response(id);
    assert_eq!(done["result"]["stopReason"], "end_turn", "{done}");
    let reqs = env.requests();
    assert_eq!(reqs.len(), 2);
    assert!(reqs[1].to_string().contains("[deck boundary]"), "{}", reqs[1]);
    c.finish();
}

#[test]
fn a_provider_rate_limit_still_retries() {
    let env = Env::new(
        "deck-provider-429",
        json!({ "steps": [
            { "status": 429, "retry_after": 0, "body": r#"{"error":{"message":"slow down","type":"rate_limit"}}"# },
            { "text": "recovered" }
        ] }),
    );
    let out = env.run(&["run", "--no-learn", "hi"]);
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(env.requests().len(), 2, "429 keeps the retry path");
}

#[test]
fn boundary_wait_times_out_on_its_own_deadline() {
    let server = boundary::Server::start();
    server.pending("a1f3", "index.crates.io:443");
    let env = Env::new(
        "deck-timeout",
        json!({ "steps": [
            { "call": "boundary_wait", "args": { "id": "a1f3", "timeout_s": 1 } },
            { "text": "done" }
        ] }),
    );
    let started = Instant::now();
    let out = env.run_with(&[("DECK_BOUNDARY_URL", server.url().as_str())], &["run", "--no-learn", "wait"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(started.elapsed() >= Duration::from_millis(900), "{:?}", started.elapsed());
    let tool = last_tool_result(&env.requests()[1]);
    assert!(tool.contains("timeout after 1s"), "{tool}");
}

#[test]
fn a_notification_without_the_capability_is_ignored() {
    let env = Env::new("deck-no-cap", json!({ "steps": [{ "text": "ok" }] }));
    env.config("[router]\nmode = \"local\"\n");
    let (mut c, sid) = acp(&env, &[]);
    let (init, _) = c.call("initialize", json!({ "protocolVersion": 1, "clientCapabilities": {} }));
    assert!(init["result"]["agentCapabilities"]["_meta"]["deck"].is_null(), "{init}");
    assert!(init["result"]["agentCapabilities"]["_meta"]["com.sprawlcommons/deck"].is_null(), "{init}");
    c.notify(
        "_deck/boundary",
        json!({ "approval_id": "a1f3", "destination": "index.crates.io:443", "outcome": "allowed", "scope": "task" }),
    );
    let (done, _) = c.call(
        "session/prompt",
        json!({
            "sessionId": sid,
            "prompt": [{ "type": "text", "text": "first" }]
        }),
    );
    assert_eq!(done["result"]["stopReason"], "end_turn");
    assert!(!env.requests().last().unwrap().to_string().contains("[deck boundary]"));
    c.finish();
}

#[test]
fn a_managed_guest_ignores_a_guest_search_key() {
    let env = Env::new(
        "deck-optional-search",
        json!({ "steps": [
            { "call": "web_search", "args": { "query": "rust" } },
            { "text": "done" }
        ] }),
    );
    env.config("[router]\nmode = \"off\"\n\n[tools.web]\nsearch = \"exa\"\n");
    let out = env.run_with(
        &[("DECK_MANAGED", "1"), ("OPENAI_API_KEY", "deck-placeholder"), ("EXA_API_KEY", "guest-supplied")],
        &["run", "--no-learn", "search the web"],
    );
    assert!(out.status.success(), "{}", text(&out));
    let tool = last_tool_result(&env.requests()[1]);
    assert!(tool.contains("EXA_API_KEY is not set"), "a guest key is not a grant: {tool}");
    assert!(tool.contains("credentials are host-owned"), "the error tells the truth: {tool}");
    assert!(!tool.contains("guest-supplied"), "the value never reaches the model: {tool}");
}

#[test]
fn a_managed_guest_ignores_a_guest_jev_key() {
    let env = Env::new("deck-optional-jev", json!({ "steps": [{ "text": "local route" }] }));
    env.config(&format!("[router]\nmode = \"auto\"\njev_url = \"http://127.0.0.1:{}/v1/systemone\"\n", env.port));
    let out = env.run_with(
        &[("DECK_MANAGED", "1"), ("OPENAI_API_KEY", "deck-placeholder"), ("TYPESAFE_API_KEY", "guest-supplied")],
        &["run", "--json", "--no-learn", "hi"],
    );
    assert!(out.status.success(), "{}", text(&out));
    let route: serde_json::Value = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .find(|e: &serde_json::Value| e["event"] == "route")
        .expect("route event");
    assert_eq!(route["source"], "local");
    assert!(!route["note"].as_str().unwrap_or_default().contains("jev failed"), "{route}");
    assert!(!env.home.join("jev.log").exists(), "no Jev call was made with a guest key");
}

/// Whether any file under `dir` holds `needle`: a guest credential must not
/// land in the task's writable disk, whatever the command prints.
fn holds(dir: &std::path::Path, needle: &str) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else { return false };
    entries.flatten().any(|entry| {
        let path = entry.path();
        if path.is_dir() {
            holds(&path, needle)
        } else {
            std::fs::read(&path).is_ok_and(|bytes| String::from_utf8_lossy(&bytes).contains(needle))
        }
    })
}

#[test]
fn a_managed_guest_reports_environment_values_as_placeholders() {
    let env = Env::new("deck-secrets-status", json!({ "steps": [] }));
    let state = env.dir.join("secretspec-state");
    let config = env.dir.join("secretspec-config");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::create_dir_all(&config).unwrap();
    let out = env.run_with(
        &[
            ("DECK_MANAGED", "1"),
            ("OPENAI_API_KEY", "deck-placeholder"),
            // If anything reaches for a keyring, Linux fails the provider
            // loudly here instead of silently finding a store.
            ("XDG_STATE_HOME", state.to_str().unwrap()),
            ("XDG_CONFIG_HOME", config.to_str().unwrap()),
            ("DBUS_SESSION_BUS_ADDRESS", "unix:path=/nonexistent-construct-test"),
        ],
        &["secrets", "status"],
    );
    assert!(out.status.success(), "{}", text(&out));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        stdout,
        "✓ OPENAI_API_KEY (placeholder from environment)\n\
         · OPENROUTER_API_KEY\n\
         · TYPESAFE_API_KEY\n\
         · EXA_API_KEY\n\
         · BRAVE_API_KEY\n\
         · TAVILY_API_KEY\n",
        "name and source only, never the value: {stdout}"
    );
    let shown = text(&out);
    assert!(!shown.contains("deck-placeholder"), "a value must never be printed: {shown}");
    assert!(!shown.contains("keyring"), "no keyring in a managed guest: {shown}");
    assert!(!shown.contains("secretspec"), "no secretspec diagnostics in a managed guest: {shown}");
}

#[test]
fn a_managed_guest_refuses_to_store_a_secret() {
    let env = Env::new("deck-secrets-set", json!({ "steps": [] }));
    let out = env.run_with(
        &[("DECK_MANAGED", "1"), ("OPENAI_API_KEY", "deck-placeholder")],
        &["secrets", "set", "OPENAI_API_KEY", "hunter2"],
    );
    assert!(!out.status.success(), "secrets set must fail in a guest: {}", text(&out));
    let shown = text(&out);
    assert!(shown.contains("host-owned in this Deck task"), "{}", shown);
    assert!(shown.contains("Deck's host-side credential"), "{}", shown);
    assert!(!shown.contains("stored"), "nothing was stored: {shown}");
    assert!(!shown.contains("keyring"), "the keyring must not be touched: {shown}");
    assert!(!holds(&env.home, "hunter2"), "a guest must not persist the value");
}

#[test]
fn a_host_still_reports_an_environment_secret_without_a_source_suffix() {
    let env = Env::new("deck-secrets-host", json!({ "steps": [] }));
    let out = env.run(&["secrets", "status"]);
    assert!(out.status.success(), "{}", text(&out));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.lines().any(|line| line == "✓ OPENAI_API_KEY"), "host output is unchanged: {stdout}");
    assert!(!stdout.contains("placeholder"), "a host has no Deck placeholder: {stdout}");
}
