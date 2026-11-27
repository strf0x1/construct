//! Contract: a `session/cancel` the ACP server reads after a `session/prompt`
//! but before that prompt's turn task has started still stops the turn. The
//! hub (#362) sends the two lines back to back when a coordinator cancels a
//! send it just made (#366), and the peer used to clear the flag as the
//! turn began, so the work ran to completion.
//!
//! The ordering is forced, not timed: the server runs on this test's
//! single-threaded runtime and both lines are queued before it is polled
//! again, so it reads the cancel before the spawned turn can run.

use construct_core::acp::server::{Input, serve};
use construct_core::config::{Config, Paths};
use construct_core::tools::ToolSources;
use serde_json::{Value, json};
use std::net::TcpListener;
use std::time::Duration;
use tokio::sync::mpsc;

fn request(id: u64, method: &str, params: Value) -> Input {
    Input::Line(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }).to_string())
}

/// Lines from the server until the response to `id`.
async fn answer(out: &mut mpsc::UnboundedReceiver<String>, id: u64, seen: &mut Vec<Value>) -> Value {
    let wait = async {
        while let Some(line) = out.recv().await {
            let v: Value = serde_json::from_str(&line).unwrap();
            seen.push(v.clone());
            if v["id"] == id && v.get("method").is_none() {
                return v;
            }
        }
        panic!("the server closed before answering {id}");
    };
    tokio::time::timeout(Duration::from_secs(10), wait)
        .await
        .unwrap_or_else(|_| panic!("no answer to {id}; saw {seen:#?}"))
}

#[tokio::test]
async fn a_cancel_read_before_the_turn_starts_stops_it() {
    let root = std::env::temp_dir().join(format!("construct-acp-cancel-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let work = root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    // The model: a socket nobody answers. A turn that reached it would hang
    // here and the test would time out; one that was cancelled never dials.
    let model = TcpListener::bind("127.0.0.1:0").unwrap();
    model.set_nonblocking(true).unwrap();
    let cfg = Config {
        base_url: format!("http://{}/v1", model.local_addr().unwrap()),
        api_key_secret: String::new(),
        router: construct_core::config::RouterConfig { mode: "off".into(), ..Default::default() },
        learn: construct_core::config::LearnConfig { enabled: false, ..Default::default() },
        ..Default::default()
    };
    let paths = Paths { home: root.join("home") };

    let (input, rx) = mpsc::unbounded_channel();
    let (tx, mut out) = mpsc::unbounded_channel();
    let server = tokio::spawn(serve(cfg, paths, ToolSources::default(), rx, tx));
    let mut seen = vec![];
    input.send(request(1, "initialize", json!({ "protocolVersion": 1 }))).unwrap();
    answer(&mut out, 1, &mut seen).await;
    input.send(request(2, "session/new", json!({ "cwd": work, "mcpServers": [] }))).unwrap();
    let opened = answer(&mut out, 2, &mut seen).await;
    let sid = opened["result"]["sessionId"].as_str().unwrap_or_else(|| panic!("{opened:#}")).to_string();

    // Back to back, with no await between: the server reads both in one go.
    let blocks = json!([{ "type": "text", "text": "run something slow" }]);
    input.send(request(3, "session/prompt", json!({ "sessionId": sid, "prompt": blocks }))).unwrap();
    let cancel = json!({ "jsonrpc": "2.0", "method": "session/cancel", "params": { "sessionId": sid } });
    input.send(Input::Line(cancel.to_string())).unwrap();

    let reply = answer(&mut out, 3, &mut seen).await;
    assert_eq!(reply["result"]["stopReason"], "cancelled", "{reply:#}");
    assert!(model.accept().is_err(), "the cancelled turn called the model");

    // The flag is cleared for the next prompt: it reaches the model.
    input.send(request(4, "session/prompt", json!({ "sessionId": sid, "prompt": blocks }))).unwrap();
    let dialed = async {
        loop {
            if model.accept().is_ok() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(10), dialed).await.expect("the next prompt never called the model");

    drop(input);
    server.abort();
    let _ = std::fs::remove_dir_all(&root);
}
