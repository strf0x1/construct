//! The `developer` tool in a Deck task with a repository: it publishes through
//! a stub of Deck's GitHub gateway (contract 1.2.0 §11), never through host
//! git or gh. The stub parses the publish framing and the bundle header the
//! way Deck's gateway does, and every request head the real binary sent is
//! checked against the vendored contract schemas.

use crate::acp::Client;
use crate::acp_payloads::{check, validators};
use crate::common::*;
use construct::acp::map;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};

/// One request the stub received: its path, its JSON head and, for a publish,
/// the bundle's header lines.
#[derive(Clone, Debug)]
struct Seen {
    path: String,
    head: Value,
    bundle: Vec<String>,
}

struct Gateway {
    port: u16,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl Gateway {
    /// `branches` is what `refs` reports; `publish` is the reply to a publish
    /// as `(status, body)`, the published commit filled in when it is 200.
    fn start(branches: Value, publish: (u16, Value)) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let (log, branches, publish) = (log.clone(), branches.clone(), publish.clone());
                std::thread::spawn(move || serve(stream, &log, &branches, &publish));
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

fn serve(stream: std::net::TcpStream, log: &Mutex<Vec<Seen>>, branches: &Value, publish: &(u16, Value)) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let path = line.split(' ').nth(1).unwrap_or_default().to_string();
    let mut length = 0;
    loop {
        let mut header = String::new();
        reader.read_line(&mut header).unwrap();
        if header.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse().unwrap();
        }
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).unwrap();
    // The JSON head ends where its value ends (contract 1.2.0 §11.3).
    let mut values = serde_json::Deserializer::from_slice(&body).into_iter::<Value>();
    let head = values.next().unwrap().unwrap();
    let rest = &body[values.byte_offset()..];
    let mut bundle = Vec::new();
    if path == "/operations/publish" {
        let n = u32::from_be_bytes(rest[..4].try_into().unwrap()) as usize;
        assert_eq!(rest.len(), 4 + n, "one length-framed bundle");
        let header = String::from_utf8_lossy(&rest[4..]);
        bundle = header.lines().take_while(|l| !l.is_empty()).map(str::to_string).collect();
    }
    log.lock().unwrap().push(Seen { path: path.clone(), head: head.clone(), bundle: bundle.clone() });
    let pr = json!({
        "number": 7, "title": head["title"].as_str().unwrap_or("Add b"), "state": "open",
        "draft": head["draft"].as_bool().unwrap_or(true), "merged": false, "head": "deck/t-1",
        "head_commit": "c".repeat(40), "base": "main", "mergeable": null,
        "html_url": "https://github.com/owner/repo/pull/7", "domain": "owner/repo#deck/t-1",
    });
    let (status, reply) = match path.as_str() {
        "/operations/refs" => (
            200,
            json!({
                "repository": "owner/repo", "base": "main", "task_branch": "deck/t-1",
                "writable": ["deck/t-1"], "branches": branches, "domain": "owner/repo",
            }),
        ),
        "/operations/publish" if publish.0 == 200 => {
            let commit = bundle.iter().find_map(|l| l.strip_suffix(" refs/heads/deck/t-1")).unwrap().to_string();
            let mut reply = publish.1.clone();
            reply["commit"] = json!(commit);
            (200, reply)
        }
        "/operations/publish" => publish.clone(),
        "/operations/pull_request" | "/operations/pull_request/update" => (200, pr),
        "/operations/pull_request/review" => {
            let mut review = pr;
            review["checks"] =
                json!({ "total": 1, "runs": [{ "name": "ci", "status": "completed", "conclusion": "success" }] });
            (200, review)
        }
        _ => (
            404,
            json!({ "error": { "message": "no", "type": "deck_not_permitted", "code": "deck_not_permitted" }, "retryable": false }),
        ),
    };
    let text = reply.to_string();
    let mut stream = stream;
    let _ = write!(
        stream,
        "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{text}",
        text.len()
    );
}

fn git(work: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(["-c", "core.hooksPath=/dev/null", "-c", "commit.gpgsign=false"])
        .args(args)
        .current_dir(work)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", text(&out));
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// A checkout like the one Deck prepares in the guest: `main`, then the task
/// branch with one more commit. Returns (base, task tip).
fn checkout(env: &Env) -> (String, String) {
    env.config("[learn]\nenabled = false\n[router]\nmode = \"local\"\n");
    git(&env.work, &["init", "-b", "main"]);
    git(&env.work, &["config", "user.name", "Deck User"]);
    git(&env.work, &["config", "user.email", "user@example.invalid"]);
    std::fs::write(env.work.join("a.txt"), "a\n").unwrap();
    git(&env.work, &["add", "a.txt"]);
    git(&env.work, &["commit", "-m", "base"]);
    let base = git(&env.work, &["rev-parse", "HEAD"]);
    git(&env.work, &["switch", "-c", "deck/t-1"]);
    std::fs::write(env.work.join("b.txt"), "b\n").unwrap();
    git(&env.work, &["add", "b.txt"]);
    git(&env.work, &["commit", "-m", "Add b"]);
    (base, git(&env.work, &["rev-parse", "HEAD"]))
}

fn calls(steps: &[Value]) -> Value {
    let mut all: Vec<Value> = steps.iter().map(|args| json!({ "call": "developer", "args": args })).collect();
    all.push(json!({ "text": "done" }));
    json!({ "steps": all })
}

fn deck(gateway: &Gateway) -> [(&'static str, String); 2] {
    [("DECK_GITHUB_URL", gateway.url()), ("DECK_MANAGED", "1".into())]
}

fn run(env: &Env, extra: &[(&'static str, String)]) {
    let extra: Vec<(&str, &str)> = extra.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let out = env.run_with(&extra, &["run", "--no-learn", "publish it"]);
    assert!(out.status.success(), "{}", text(&out));
}

#[test]
fn a_deck_task_pushes_opens_a_draft_and_reads_checks_without_asking() {
    let env = Env::new(
        "deck-gh-flow",
        calls(&[
            json!({ "operation": "push" }),
            json!({ "operation": "pr_create", "title": "Add b", "body": "Adds b." }),
            json!({ "operation": "pr_review", "number": 7 }),
            json!({ "operation": "pr_update", "number": 7, "draft": false }),
        ]),
    );
    let (base, tip) = checkout(&env);
    // The task branch already holds `base` on GitHub, so the bundle carries
    // only the new commit and names `base` as its prerequisite.
    let gateway = Gateway::start(
        json!([{ "branch": "main", "commit": base, "task_branch": false }, { "branch": "deck/t-1", "commit": base, "task_branch": true }]),
        (
            200,
            json!({ "repository": "owner/repo", "branch": "deck/t-1", "previous": base, "created": false, "domain": "owner/repo#deck/t-1" }),
        ),
    );
    run(&env, &deck(&gateway));

    let reqs = env.requests();
    let pushed = last_tool_result(&reqs[1]);
    assert!(
        pushed.contains(&format!("Published {tip} to owner/repo#deck/t-1 (fast-forwarded from {base})")),
        "{pushed}"
    );
    let created = last_tool_result(&reqs[2]);
    assert!(created.contains("Pull request #7 \"Add b\" (draft, deck/t-1 -> main)"), "{created}");
    let reviewed = last_tool_result(&reqs[3]);
    assert!(reviewed.contains("- ci: success"), "{reviewed}");
    assert!(!last_tool_result(&reqs[4]).contains("denied"), "Deck mode asks Construct nothing");

    let seen = gateway.seen();
    let paths: Vec<&str> = seen.iter().map(|s| s.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "/operations/refs",
            "/operations/publish",
            "/operations/refs",
            "/operations/pull_request",
            "/operations/pull_request/review",
            "/operations/refs",
            "/operations/pull_request/update"
        ]
    );
    let publish = &seen[1];
    assert_eq!(
        publish.bundle,
        ["# v2 git bundle".to_string(), format!("-{base} base"), format!("{tip} refs/heads/deck/t-1")]
    );
    assert_eq!(publish.head["summary"], json!({ "subject": "Add b", "count": 1 }));
    assert_eq!(
        seen[3].head,
        json!({ "operation": "create_pr", "head": "deck/t-1", "base": "main", "title": "Add b", "body": "Adds b.", "draft": true })
    );
    assert_eq!(seen[6].head, json!({ "operation": "update_pr", "number": 7, "draft": false }));

    let schemas = validators();
    let schema = |path: &str| match path {
        "/operations/refs" => "github-refs-request",
        "/operations/publish" => "github-publish-request",
        "/operations/pull_request" => "github-create-pr-request",
        "/operations/pull_request/update" => "github-update-pr-request",
        _ => "github-review-request",
    };
    for s in &seen {
        check(&schemas, schema(&s.path), &s.head);
    }
}

#[test]
fn a_rewritten_branch_is_reported_as_rebase_onto_and_not_retried() {
    let env = Env::new("deck-gh-rewrite", calls(&[json!({ "operation": "push" })]));
    checkout(&env);
    let elsewhere = "d".repeat(40);
    let gateway = Gateway::start(
        json!([{ "branch": "deck/t-1", "commit": elsewhere, "task_branch": true }]),
        (
            403,
            json!({
                "error": { "message": "the publish would rewrite deck/t-1", "type": "deck_github_refused", "code": "deck_github_refused" },
                "retryable": false, "current": elsewhere,
            }),
        ),
    );
    run(&env, &deck(&gateway));
    let result = last_tool_result(&env.requests()[1]);
    assert!(result.contains(&format!("rebase onto {elsewhere} and push again")), "{result}");
    // A commit the local history does not know: the whole branch was sent and
    // the gateway, not the tool, refused it. Once.
    let publishes: Vec<Seen> = gateway.seen().into_iter().filter(|s| s.path == "/operations/publish").collect();
    assert_eq!(publishes.len(), 1);
    assert!(!publishes[0].bundle.iter().any(|l| l.starts_with('-')), "{:?}", publishes[0].bundle);
}

#[test]
fn issue_create_is_refused_and_nothing_reaches_the_gateway_or_gh() {
    let env = Env::new(
        "deck-gh-issue",
        calls(&[json!({ "operation": "issue_create", "repo": "owner/repo", "title": "t", "body": "" })]),
    );
    checkout(&env);
    let gateway = Gateway::start(json!([]), (200, json!({})));
    run(&env, &deck(&gateway));
    let result = last_tool_result(&env.requests()[1]);
    assert!(result.contains("issue_create is not available in a Deck task"), "{result}");
    assert!(gateway.seen().is_empty());
}

#[test]
fn outside_deck_mode_a_push_still_needs_constructs_own_approval() {
    let env = Env::new("deck-gh-unmanaged", calls(&[json!({ "operation": "push" })]));
    checkout(&env);
    let gateway = Gateway::start(json!([]), (200, json!({})));
    run(&env, &[("DECK_GITHUB_URL", gateway.url())]);
    let result = last_tool_result(&env.requests()[1]);
    assert!(result.contains("not approved"), "with nobody to ask, the push is not sent: {result}");
    assert!(!gateway.seen().iter().any(|s| s.path == "/operations/publish"));
}

#[test]
fn initialize_advertises_github_operations_under_the_current_key_only() {
    let env = Env::new("deck-gh-cap", json!({ "steps": [{ "text": "ok" }] }));
    let gateway = Gateway::start(json!([]), (200, json!({})));
    let mut c = Client::start_with(&env, &[("DECK_GITHUB_URL", gateway.url().as_str())]);
    let (init, _) = c.call("initialize", json!({ "protocolVersion": 1, "clientCapabilities": {} }));
    let meta = &init["result"]["agentCapabilities"]["_meta"];
    assert_eq!(meta[map::DECK_KEY][map::GITHUB_OPERATIONS], json!(map::GITHUB_OPERATIONS_VERSION), "{init}");
    assert!(meta[map::LEGACY_DECK_KEY].get(map::GITHUB_OPERATIONS).is_none(), "{init}");
    check(&validators(), "agent-capabilities", meta);
    c.finish();

    let plain = Env::new("deck-gh-nocap", json!({ "steps": [{ "text": "ok" }] }));
    let mut c = Client::start_with(&plain, &[]);
    let (init, _) = c.call("initialize", json!({ "protocolVersion": 1, "clientCapabilities": {} }));
    assert!(init["result"]["agentCapabilities"]["_meta"].get(map::DECK_KEY).is_none(), "{init}");
    c.finish();
}
