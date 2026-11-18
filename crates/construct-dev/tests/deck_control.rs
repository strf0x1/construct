//! A Deck coordinator's `deck` tool end to end (#222, contract 1.5.0 §13):
//! the real binary, a scripted model and a stub of Deck's control gateway.
//! The tool exists only when `DECK_CONTROL_URL` names a loopback gateway,
//! carries the bearer header exactly when `DECK_CONTROL_TOKEN` is set, never
//! retries a refusal, and the token reaches no tool result, journal row,
//! model request or output line.

use crate::common::*;
use construct::tools::control::NAME;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

const TOKEN: &str = "dct_q8Zr1vKcY3nT0fWb6LmE2pXhS9aJ4uGd";
const WORKER: &str = "01JD3W0RKER0000000000000001";

/// One request the stub received: its `Authorization` headers and body.
#[derive(Clone, Debug)]
struct Seen {
    authorization: Vec<String>,
    body: Value,
}

/// Deck's control gateway, answering by operation: `prompt` is busy. A 1.4
/// gateway's `tasks` lists one worker and no projects; a 1.5 one lists no
/// tasks and names the coordinator's one project.
struct Gateway {
    port: u16,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl Gateway {
    fn start() -> Self {
        Self::speaking(false)
    }

    fn speaking(projects: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let log = log.clone();
                std::thread::spawn(move || serve(stream, &log, projects));
            }
        });
        Self { port, seen }
    }

    fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }
}

fn summary() -> Value {
    json!({
        "id": WORKER, "project": "git:abc", "title": "Fix the flaky login test", "state": "running",
        "turn": { "number": 1, "state": "idle", "stopReason": "end_turn" }, "attention": "finished",
        "pending_approvals": 0, "publication": null, "pull_request": null,
        "created_by": "01JD3C00RD1NAT0R0000000001", "last_message": "The test raced the session cookie."
    })
}

fn serve(stream: std::net::TcpStream, log: &Mutex<Vec<Seen>>, projects: bool) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let (mut length, mut authorization) = (0, vec![]);
    loop {
        let mut header = String::new();
        reader.read_line(&mut header).unwrap();
        if header.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                length = value.trim().parse().unwrap();
            }
            if name.eq_ignore_ascii_case("authorization") {
                authorization.push(value.trim().to_string());
            }
        }
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).unwrap();
    let body: Value = serde_json::from_slice(&body).unwrap();
    let (status, reply) = match body["operation"].as_str().unwrap() {
        "tasks" if projects => {
            (200, json!({ "tasks": [], "truncated": false, "projects": [{ "id": "git:abc", "name": "app" }] }))
        }
        "tasks" => (200, json!({ "tasks": [summary()], "truncated": false })),
        "create" => (200, json!({ "id": WORKER })),
        "prompt" => (
            409,
            json!({ "error": { "message": "the task's turn is working", "type": "deck_control_busy", "code": "deck_control_busy" }, "retryable": true }),
        ),
        "wait" => (200, json!({ "timed_out": false, "task": summary() })),
        _ => (200, json!({ "id": WORKER, "state": "stopped" })),
    };
    log.lock().unwrap().push(Seen { authorization, body });
    let text = reply.to_string();
    let mut out = stream;
    let _ = write!(
        out,
        "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{text}",
        text.len()
    );
}

fn deck(args: Value) -> Value {
    json!({ "call": NAME, "args": args })
}

fn tool_results(req: &Value) -> Vec<String> {
    req["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["role"] == "tool")
        .map(|m| m["content"].as_str().unwrap_or_default().to_string())
        .collect()
}

#[test]
fn a_coordinator_creates_prompts_waits_and_stops_a_worker() {
    let gateway = Gateway::start();
    let env = Env::new(
        "deck-control",
        json!({ "steps": [
            deck(json!({ "operation": "create", "project": "git:abc", "brief": "Fix the flaky login test", "title": "Fix it" })),
            deck(json!({ "operation": "prompt", "id": WORKER, "text": "Open a draft pull request." })),
            deck(json!({ "operation": "wait", "id": WORKER, "after_turn": 0, "wait_ms": 1000 })),
            deck(json!({ "operation": "tasks" })),
            deck(json!({ "operation": "stop", "id": WORKER })),
            { "text": "done" }
        ] }),
    );
    let url = gateway.url();
    let out = env
        .run_with(&[("DECK_CONTROL_URL", &url), ("DECK_CONTROL_TOKEN", TOKEN)], &["run", "--json", "--no-learn", "go"]);
    assert!(out.status.success(), "{}", text(&out));
    let reqs = env.requests();
    assert_eq!(reqs.len(), 6, "{}", text(&out));
    assert!(
        tool_names(&reqs[0]).contains(&NAME.to_string()),
        "offered from the first request: {:?}",
        tool_names(&reqs[0])
    );

    let seen = gateway.seen();
    let ops: Vec<&str> = seen.iter().map(|s| s.body["operation"].as_str().unwrap()).collect();
    assert_eq!(ops, ["create", "prompt", "wait", "tasks", "stop"], "busy is not retried");
    for s in &seen {
        assert_eq!(s.authorization, vec![format!("Bearer {TOKEN}")], "{:?}", s.body);
    }
    assert_eq!(
        seen[0].body,
        json!({ "operation": "create", "project": "git:abc", "brief": "Fix the flaky login test", "title": "Fix it" })
    );
    assert_eq!(seen[2].body, json!({ "operation": "wait", "id": WORKER, "after_turn": 0, "wait_ms": 1000 }));

    let results = tool_results(&reqs[5]);
    assert!(results[0].contains(&format!("Created task {WORKER}")), "{}", results[0]);
    assert!(
        results[1].contains("deck_control_busy (409): the task's turn is working") && results[1].contains("deck wait"),
        "{}",
        results[1]
    );
    assert!(
        results[2].starts_with("Woke:") && results[2].contains("The test raced the session cookie."),
        "{}",
        results[2]
    );
    assert!(results[3].starts_with("1 task:") && results[3].contains("needs you: finished"), "{}", results[3]);
    assert_eq!(results[4], format!("Task {WORKER} is stopped."));

    // The token is in no tool result, model request, journal row or output line.
    for req in &reqs {
        assert!(!req.to_string().contains(TOKEN), "a model request carries the token");
    }
    for row in env.journal() {
        assert!(!row.payload.to_string().contains(TOKEN), "journal row {} ({}) carries the token", row.seq, row.ty);
    }
    assert!(!text(&out).contains(TOKEN), "the output carries the token");
    let kinds: Vec<Value> = env
        .journal()
        .into_iter()
        .filter(|r| r.ty == "tool_end")
        .map(|r| json!([r.payload["name"], r.payload["ok"]]))
        .collect();
    assert_eq!(kinds[1], json!([NAME, false]), "the refusal is a tool error");
}

#[test]
fn a_coordinator_learns_its_project_and_leaves_create_s_project_to_deck() {
    let gateway = Gateway::speaking(true);
    let env = Env::new(
        "deck-control-projects",
        json!({ "steps": [
            deck(json!({ "operation": "tasks" })),
            deck(json!({ "operation": "create", "brief": "Fix the flaky login test" })),
            { "text": "done" }
        ] }),
    );
    let url = gateway.url();
    let out = env.run_with(&[("DECK_CONTROL_URL", &url)], &["run", "--json", "--no-learn", "go"]);
    assert!(out.status.success(), "{}", text(&out));
    let seen = gateway.seen();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[1].body, json!({ "operation": "create", "brief": "Fix the flaky login test" }), "no project sent");
    let results = tool_results(&env.requests()[2]);
    assert!(
        results[0].starts_with("Your projects: app (git:abc)") && results[0].ends_with("No tasks in scope."),
        "an empty listing still names the project: {}",
        results[0]
    );
    assert!(results[1].starts_with(&format!("Created task {WORKER};")), "{}", results[1]);
    let first = &env.requests()[0];
    let schema = first["tools"].as_array().unwrap().iter().find(|t| t["function"]["name"] == NAME).unwrap();
    let project = schema.pointer("/function/parameters/properties/project/description").and_then(Value::as_str);
    assert!(project.is_some_and(|d| d.contains("leave it out")), "{project:?}");
}

#[test]
fn without_a_token_no_header_is_sent() {
    let gateway = Gateway::start();
    let env =
        Env::new("deck-control-vm", json!({ "steps": [deck(json!({ "operation": "tasks" })), { "text": "done" }] }));
    let url = gateway.url();
    let out = env.run_with(&[("DECK_CONTROL_URL", &url)], &["run", "--no-learn", "go"]);
    assert!(out.status.success(), "{}", text(&out));
    let seen = gateway.seen();
    assert_eq!(seen.len(), 1);
    assert!(seen[0].authorization.is_empty(), "{:?}", seen[0].authorization);
    let result = last_tool_result(&env.requests()[1]);
    assert!(result.starts_with("1 task:") && result.contains(WORKER), "a 1.4 reply has no projects: {result}");
}

#[test]
fn without_a_usable_control_url_there_is_no_deck_tool() {
    for (name, url) in [("deck-control-none", None), ("deck-control-remote", Some("http://10.0.0.2:8080"))] {
        let env = Env::new(name, json!({ "steps": [{ "text": "done" }] }));
        let mut extra = vec![("DECK_CONTROL_TOKEN", TOKEN)];
        extra.extend(url.map(|u| ("DECK_CONTROL_URL", u)));
        let out = env.run_with(&extra, &["run", "--no-learn", "go"]);
        assert!(out.status.success(), "{}", text(&out));
        assert!(!tool_names(&env.requests()[0]).contains(&NAME.to_string()), "{name}");
    }
}
