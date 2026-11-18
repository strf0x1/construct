//! Contract: the coordinator's `deck` tool against the vendored 1.4.0
//! `control` matrix (contract §13). Every exchange is replayed through the
//! tool, with and without a bearer token, against a stub gateway that answers
//! with the exchange's reply: the request the tool sends must be the
//! exchange's own when the exchange is one this client can produce, and the
//! reply must decode, or map to its documented refusal code.

use construct::config::{MemoryConfig, Paths};
use construct::memory::Store;
use construct::sandbox::Policy;
use construct::tools::control::{self, Control, MAX_BODY, MAX_WAIT_MS, OPERATIONS, PATH, REFUSALS};
use construct::tools::{Ctx, Tool};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

fn matrix() -> Value {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let lock = std::fs::read_to_string(root.join("CONTRACT.lock")).unwrap();
    let vendored = lock.lines().find_map(|l| l.strip_prefix("vendored = ")).unwrap().trim_matches('"');
    serde_json::from_str(&std::fs::read_to_string(root.join(vendored).join("matrix.json")).unwrap()).unwrap()
}

/// One request the stub received.
#[derive(Clone, Debug)]
struct Seen {
    method: String,
    path: String,
    authorization: Vec<String>,
    body: Value,
}

/// A gateway that answers every request with one fixed reply.
struct Stub {
    url: String,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl Stub {
    fn start(status: u16, reply: Value) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                serve(stream, &log, status, &reply);
            }
        });
        Self { url, seen }
    }

    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }
}

fn serve(stream: std::net::TcpStream, log: &Mutex<Vec<Seen>>, status: u16, reply: &Value) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let mut parts = line.split(' ');
    let (method, path) = (parts.next().unwrap_or_default().to_string(), parts.next().unwrap_or_default().to_string());
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
    let body = serde_json::from_slice(&body).unwrap_or(Value::Null);
    log.lock().unwrap().push(Seen { method, path, authorization, body });
    let text = reply.to_string();
    let mut out = stream;
    let _ = write!(
        out,
        "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{text}",
        text.len()
    );
}

/// The refusal a call ended in.
async fn refused(tool: &Control, ctx: &Ctx, args: Value) -> construct::tools::ToolError {
    match tool.call(ctx, args).await {
        Ok(out) => panic!("expected a refusal, got {}", out.text),
        Err(e) => e,
    }
}

fn ctx(root: &Path) -> Ctx {
    let paths = Paths { home: root.join("home") };
    let memory = Store::new(&paths, &MemoryConfig::default());
    let (_tx, rx) = tokio::sync::watch::channel(false);
    Ctx::new(root.to_path_buf(), paths, memory, rx, None, Policy::default())
}

fn scratch(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("construct-deck-control-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root
}

#[test]
fn the_tool_speaks_the_contract_s_names_and_limits() {
    let m = matrix();
    let c = &m["control"];
    assert_eq!(c["since"], "1.4.0");
    assert_eq!((c["method"].as_str(), c["path"].as_str()), (Some("POST"), Some(PATH)));
    assert_eq!(c["environment"]["url"], construct::config::deck::CONTROL_URL);
    assert_eq!(c["environment"]["token"], construct::config::deck::CONTROL_TOKEN);
    assert_eq!(c["authorization"]["scheme"], "Bearer");
    assert_eq!(c["limits"]["body_bytes"], json!(MAX_BODY));
    assert_eq!(c["limits"]["reply_bytes"], json!(control::MAX_REPLY));
    assert_eq!(c["limits"]["wait_ms_max"], json!(MAX_WAIT_MS));
    let ops: Vec<&str> = c["operations"].as_array().unwrap().iter().map(|o| o["operation"].as_str().unwrap()).collect();
    assert_eq!(ops, OPERATIONS, "the closed set, in the contract's order");
    for host in c["url_hosts"].as_array().unwrap() {
        let url = format!("http://{}:8080", host.as_str().unwrap());
        assert_eq!(construct::config::deck::control_url_from(&url), Some(url.clone()), "{url}");
    }
    let refusals: Vec<(&str, bool)> = c["refusals"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| (r["code"].as_str().unwrap(), r["retryable"].as_bool().unwrap()))
        .collect();
    let ours: Vec<(&str, bool)> = REFUSALS.iter().map(|(code, retryable, _)| (*code, *retryable)).collect();
    assert_eq!(ours, refusals, "every refusal code and whether it is retryable");
}

#[test]
fn every_refusal_maps_to_its_code_and_says_what_to_change() {
    for (code, retryable, _) in REFUSALS {
        let body = json!({ "error": { "message": "because", "type": code, "code": code }, "retryable": retryable });
        let e = control::refusal(403, &body, true);
        assert_eq!(e.code, *code);
        assert!(e.message.starts_with(&format!("Deck refused: {code} (403): because.")), "{}", e.message);
        assert_eq!(e.data.as_ref().unwrap()["retryable"], json!(retryable));
        assert!(!e.message.contains("could not be reached"), "a refusal is never an outage: {}", e.message);
    }
    let busy = control::refusal(
        409,
        &json!({ "error": { "message": "m", "type": "deck_control_busy", "code": "deck_control_busy" }, "retryable": true }),
        true,
    );
    assert!(busy.message.contains("deck wait"), "{}", busy.message);
    let unauthorized = control::refusal(
        401,
        &json!({ "error": { "message": "m", "type": "deck_control_unauthorized", "code": "deck_control_unauthorized" }, "retryable": false }),
        false,
    );
    assert!(unauthorized.message.contains("control token") && unauthorized.message.contains("tell the user"));
}

/// What this client sends for a case's body, or `None` when it refuses the
/// body itself (an unknown field, an operation outside the set, no
/// operation, a wait over the cap): those never leave the process.
fn producible(body: &Value) -> Option<Value> {
    control::request(body).ok()
}

#[tokio::test(flavor = "multi_thread")]
async fn every_exchange_round_trips_with_and_without_the_token() {
    let root = scratch("exchanges");
    let ctx = ctx(&root);
    let m = matrix();
    let exchanges = m["control"]["exchanges"].as_array().unwrap();
    assert_eq!(exchanges.len(), 45);
    let mut sent = 0;
    for case in exchanges {
        let name = case["case"].as_str().unwrap();
        let token = case["token"].as_str().map(str::to_string);
        let status = case["expect"]["status"].as_u64().unwrap() as u16;
        let reply = case["expect"]["body"].clone();
        let stub = Stub::start(status, reply.clone());
        let tool = Control::with_token(&stub.url, token.clone());
        let result = tool.call(&ctx, case["body"].clone()).await;
        let Some(body) = producible(&case["body"]) else {
            let e = result.err().unwrap_or_else(|| panic!("{name}: an invalid body was sent"));
            assert_eq!(e.code, "invalid_argument", "{name}: {}", e.message);
            assert!(stub.seen().is_empty(), "{name}: refused here, nothing reaches Deck");
            // The reply Deck gives a client that does send it still maps.
            let mapped = control::refusal(status, &reply, false);
            assert_eq!(mapped.code, case["expect"]["code"].as_str().unwrap(), "{name}");
            continue;
        };
        sent += 1;
        let seen = stub.seen();
        assert_eq!(seen.len(), 1, "{name}: one request, never a retry");
        let seen = &seen[0];
        assert_eq!((seen.method.as_str(), seen.path.as_str()), ("POST", PATH), "{name}");
        assert_eq!(seen.body, body, "{name}");
        let header = token.as_ref().map(|t| format!("Bearer {t}"));
        assert_eq!(seen.authorization, header.iter().cloned().collect::<Vec<_>>(), "{name}: bearer exactly when set");
        // A case this client produces byte for byte: same body, same headers.
        let same_headers = case["headers"].get("authorization").and_then(Value::as_str) == header.as_deref();
        if case.get("path").is_none() && case.get("method").is_none() && same_headers {
            assert_eq!(seen.body, case["body"], "{name}: the exchange's own request");
        }
        match (status, result) {
            (200, Ok(out)) => {
                assert!(!out.text.is_empty(), "{name}");
                if let Some(t) = &token {
                    assert!(!out.text.contains(t.as_str()), "{name}: the token is never in a result");
                }
                if let Some(id) = reply.pointer("/task/id").or(reply.get("id")).and_then(Value::as_str) {
                    assert!(out.text.contains(id), "{name}: {}", out.text);
                }
            }
            (200, Err(e)) => panic!("{name}: a success decoded as {}", e.message),
            (_, Ok(out)) => panic!("{name}: a refusal read as success: {}", out.text),
            (_, Err(e)) => {
                assert_eq!(e.code, case["expect"]["code"].as_str().unwrap(), "{name}: {}", e.message);
                let message = reply.pointer("/error/message").and_then(Value::as_str).unwrap();
                assert!(e.message.contains(message), "{name}: Deck's message is shown: {}", e.message);
                if let Some(t) = &token {
                    assert!(!e.message.contains(t.as_str()), "{name}: the token is never in an error");
                }
            }
        }
    }
    assert!(sent >= 35, "most exchanges are requests this client sends: {sent}");
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refused_token_or_a_malformed_body_is_not_sent_again() {
    let root = scratch("latch");
    let ctx = ctx(&root);
    let unauthorized = json!({ "error": { "message": "missing or wrong bearer token", "type": "deck_control_unauthorized", "code": "deck_control_unauthorized" }, "retryable": false });
    let stub = Stub::start(401, unauthorized);
    let tool = Control::with_token(&stub.url, Some("dct_not_the_task_token_000000000000".into()));
    for args in [json!({ "operation": "tasks" }), json!({ "operation": "task", "id": "w1" })] {
        let e = refused(&tool, &ctx, args).await;
        assert_eq!(e.code, "deck_control_unauthorized", "{}", e.message);
    }
    assert_eq!(stub.seen().len(), 1, "after a refused token nothing more is sent");
    let bad = json!({ "error": { "message": "no", "type": "deck_control_bad_request", "code": "deck_control_bad_request" }, "retryable": false });
    let stub = Stub::start(400, bad);
    let tool = Control::with_token(&stub.url, None);
    let args = json!({ "operation": "task", "id": "w1" });
    for _ in 0..2 {
        assert_eq!(refused(&tool, &ctx, args.clone()).await.code, "deck_control_bad_request");
    }
    refused(&tool, &ctx, json!({ "operation": "task", "id": "w2" })).await;
    assert_eq!(stub.seen().len(), 2, "the identical body is refused here; a changed one is sent");
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test(flavor = "multi_thread")]
async fn busy_is_reported_once_and_never_retried() {
    let root = scratch("busy");
    let ctx = ctx(&root);
    let busy = json!({ "error": { "message": "the task's turn is working", "type": "deck_control_busy", "code": "deck_control_busy" }, "retryable": true });
    let stub = Stub::start(409, busy);
    let tool = Control::with_token(&stub.url, None);
    let e = refused(&tool, &ctx, json!({ "operation": "prompt", "id": "w1", "text": "go" })).await;
    assert_eq!(e.code, "deck_control_busy");
    assert!(e.message.contains("the task's turn is working") && e.message.contains("deck wait"), "{}", e.message);
    assert_eq!(stub.seen().len(), 1);
    let _ = std::fs::remove_dir_all(&root);
}
