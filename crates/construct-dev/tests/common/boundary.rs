//! A tiny Deck boundary endpoint for the e2e tests: events, long-polled
//! approvals and withdrawal on plain HTTP over loopback. It records what was
//! asked so a test can assert the cursor and wait it sent.

use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub struct Approval {
    pub id: String,
    pub destination: String,
    pub state: String,
    pub scope: Option<String>,
}

#[derive(Default)]
struct State {
    seq: u64,
    events: Vec<Value>,
    approvals: HashMap<String, Approval>,
    /// An event added once the first `/events` response has been built, so it
    /// lands after the tool's cursor probe rather than before it.
    arm: Option<Approval>,
    log: Vec<String>,
}

pub struct Server {
    state: Arc<Mutex<State>>,
    port: u16,
}

impl Server {
    pub fn start() -> Arc<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let state = Arc::new(Mutex::new(State::default()));
        let shared = state.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let state = shared.clone();
                std::thread::spawn(move || handle(stream, &state));
            }
        });
        Arc::new(Self { state, port })
    }

    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// An approval that exists before the tool runs, and is still pending.
    pub fn pending(&self, id: &str, destination: &str) {
        self.add(Approval { id: id.into(), destination: destination.into(), state: "pending".into(), scope: None });
    }

    /// Add the block once the next `/events` call has answered: the cursor
    /// probe sees the past, the post-tool read sees this.
    pub fn arm(&self, id: &str, destination: &str, state: &str, scope: Option<&str>) {
        let mut s = self.state.lock().unwrap();
        s.arm = Some(Approval {
            id: id.into(),
            destination: destination.into(),
            state: state.into(),
            scope: scope.map(str::to_string),
        });
    }

    /// Decide `id` now.
    pub fn resolve(&self, id: &str, state: &str, scope: Option<&str>) {
        resolve(&mut self.state.lock().unwrap(), id, state, scope);
    }

    /// Decide `id` after `delay`, as a user would while the agent waits.
    pub fn resolve_after(&self, id: &str, state: &str, scope: Option<&str>, delay: Duration) {
        let state_arc = self.state.clone();
        let id = id.to_string();
        let state = state.to_string();
        let scope = scope.map(str::to_string);
        std::thread::spawn(move || {
            std::thread::sleep(delay);
            resolve(&mut state_arc.lock().unwrap(), &id, &state, scope.as_deref());
        });
    }

    /// Every request line, in arrival order.
    pub fn requests(&self) -> Vec<String> {
        self.state.lock().unwrap().log.clone()
    }

    pub fn approval(&self, id: &str) -> Option<Approval> {
        self.state.lock().unwrap().approvals.get(id).cloned()
    }

    fn add(&self, approval: Approval) {
        add(&mut self.state.lock().unwrap(), approval);
    }
}

fn add(state: &mut State, approval: Approval) {
    state.seq += 1;
    state.events.push(json!({
        "seq": state.seq,
        "approval_id": approval.id,
        "destination": approval.destination,
        "state": approval.state,
        "scope": approval.scope,
    }));
    state.approvals.insert(approval.id.clone(), approval);
}

fn resolve(state: &mut State, id: &str, state_name: &str, scope: Option<&str>) {
    let Some(approval) = state.approvals.get(id).cloned() else {
        return;
    };
    add(state, Approval { state: state_name.into(), scope: scope.map(str::to_string), ..approval });
}

fn handle(mut stream: TcpStream, state: &Arc<Mutex<State>>) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut head = String::new();
    if reader.read_line(&mut head).is_err() {
        return;
    }
    let mut len = 0usize;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            break;
        }
        if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            len = v.trim().parse().unwrap_or(0);
        }
        if line == "\r\n" {
            break;
        }
    }
    if len > 0 {
        let mut body = vec![0u8; len];
        let _ = reader.read_exact(&mut body);
    }
    let mut parts = head.split_whitespace();
    let method = parts.next().unwrap_or_default();
    let target = parts.next().unwrap_or_default();
    state.lock().unwrap().log.push(format!("{method} {target}"));
    let (status, body) = route(method, target, state);
    let _ =
        write(&mut stream, status, &[("content-type", "application/json"), ("connection", "close")], body.as_bytes());
}

fn route(method: &str, target: &str, state: &Arc<Mutex<State>>) -> (u16, String) {
    let path = target.split('?').next().unwrap_or_default();
    if method == "GET" && path == "/events" {
        let after = query(target, "after").and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
        let mut s = state.lock().unwrap();
        let body =
            json!(s.events.iter().filter(|e| e["seq"].as_u64().unwrap_or(0) > after).cloned().collect::<Vec<_>>());
        if let Some(arm) = s.arm.take() {
            add(&mut s, arm);
        }
        return (200, body.to_string());
    }
    if method == "GET"
        && let Some(id) = path.strip_prefix("/approvals/")
    {
        let wait = query(target, "wait").and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
        let deadline = Instant::now() + Duration::from_secs(wait);
        loop {
            let s = state.lock().unwrap();
            if let Some(a) = s.approvals.get(id) {
                if wait == 0 || s_decision(&s, id) {
                    return (200, approval_json(a));
                }
            } else {
                return (404, json!({ "error": "no such approval" }).to_string());
            }
            drop(s);
            if Instant::now() >= deadline {
                let s = state.lock().unwrap();
                let body = s.approvals.get(id).map(|a| approval_body(a, "pending")).unwrap_or_else(|| "{}".into());
                return (200, body);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    if method == "POST"
        && let Some(id) = path.strip_prefix("/approvals/")
        && let Some(id) = id.strip_suffix("/withdraw")
    {
        let mut s = state.lock().unwrap();
        resolve(&mut s, id, "withdrawn", None);
        return (200, "{}".into());
    }
    (404, json!({ "error": "not found" }).to_string())
}

fn s_decision(state: &State, id: &str) -> bool {
    state.approvals.get(id).is_some_and(|a| a.state != "pending")
}

fn approval_body(a: &Approval, state: &str) -> String {
    json!({
        "approval_id": a.id,
        "destination": a.destination,
        "state": state,
        "scope": a.scope,
    })
    .to_string()
}

fn approval_json(a: &Approval) -> String {
    approval_body(a, &a.state)
}

fn query<'a>(target: &'a str, key: &str) -> Option<&'a str> {
    target.split('?').nth(1)?.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        (k == key).then_some(v)
    })
}

fn write(stream: &mut TcpStream, status: u16, headers: &[(&str, &str)], body: &[u8]) -> std::io::Result<()> {
    write!(stream, "HTTP/1.1 {status} X\r\n")?;
    for (k, v) in headers {
        write!(stream, "{k}: {v}\r\n")?;
    }
    write!(stream, "content-length: {}\r\n\r\n", body.len())?;
    stream.write_all(body)?;
    stream.flush()
}
