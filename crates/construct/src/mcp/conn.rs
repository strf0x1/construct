//! One running MCP server over stdio: spawn it under the sandbox, initialize,
//! list its tools, and multiplex requests by id.
//!
//! The design choice a reader must know: one reader task owns the server's
//! stdout. It delivers responses, answers the server's own requests (`ping`
//! with `{}`, anything else with "method not found"), and when stdout ends it
//! marks the connection dead and fails every pending request at once, so a
//! crashed server costs a call an error, not a timeout. `dead` lives under
//! the same lock as `pending`, so no request can slip in after the drain.

use crate::config::McpServer;
use crate::sandbox::Policy;
use anyhow::{Context, Result, anyhow, bail, ensure};
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin};
use tokio::sync::{Mutex, Notify, oneshot};

/// Protocol versions this client speaks; it asks for the first.
const VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];
/// Startup requests (initialize, tools/list) get this long.
const START_TIMEOUT: Duration = Duration::from_secs(30);
/// `tools/list` pages followed before giving up on a server that never ends.
const MAX_PAGES: usize = 100;
/// The tail of the server's stderr kept for a startup error.
const STDERR_TAIL: usize = 4096;
/// After stdin closes, how long a server gets to exit before SIGTERM.
const GRACE: Duration = Duration::from_secs(2);

type Stdin = Arc<Mutex<ChildStdin>>;

struct Waiter {
    tx: oneshot::Sender<Value>,
    progress: Arc<Notify>,
}

#[derive(Default)]
struct Inner {
    pending: HashMap<u64, Waiter>,
    dead: bool,
}

#[derive(Default)]
struct State(std::sync::Mutex<Inner>);

impl State {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        // A poisoned lock means a panic mid-insert; the map is still usable.
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
    /// False once the server is gone: the caller must not wait.
    fn insert(&self, id: u64, w: Waiter) -> bool {
        let mut s = self.lock();
        if s.dead {
            return false;
        }
        s.pending.insert(id, w);
        true
    }
    fn take(&self, id: u64) -> Option<Waiter> {
        self.lock().pending.remove(&id)
    }
    fn progress(&self, id: u64) {
        if let Some(w) = self.lock().pending.get(&id) {
            w.progress.notify_one();
        }
    }
    /// The server's stdout ended: dropping every sender fails every waiter.
    fn close(&self) {
        let mut s = self.lock();
        s.dead = true;
        s.pending.clear();
    }
}

pub(super) struct Conn {
    stdin: Stdin,
    state: Arc<State>,
    next: AtomicU64,
    pub tools: Vec<Value>,
    child: Option<Child>,
    stderr: Arc<std::sync::Mutex<VecDeque<u8>>>,
    stderr_done: Option<tokio::task::JoinHandle<()>>,
}

impl Conn {
    pub fn alive(&self) -> bool {
        !self.state.lock().dead
    }

    /// Send `method` and wait for its response. The wait is `idle` long, and
    /// starts over on every progress notification for this request. A request
    /// that times out or is dropped (the turn was interrupted) leaves no
    /// pending entry and tells the server with `notifications/cancelled`.
    pub async fn request(&self, method: &str, mut params: Value, idle: Duration) -> Result<Value> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, mut rx) = oneshot::channel();
        let progress = Arc::new(Notify::new());
        if !self.state.insert(id, Waiter { tx, progress: progress.clone() }) {
            bail!("MCP server closed");
        }
        let mut guard = Guard { id, method: method.to_string(), state: self.state.clone(), stdin: self.stdin.clone() };
        if method == "tools/call" {
            params["_meta"] = json!({ "progressToken": id });
        }
        write(&self.stdin, &json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })).await?;
        let v = loop {
            tokio::select! {
                r = &mut rx => break r.map_err(|_| {
                    guard.method.clear(); // nothing to cancel on a dead server
                    anyhow!("MCP server closed")
                })?,
                _ = progress.notified() => continue,
                _ = tokio::time::sleep(idle) => bail!("{method} timed out after {}s with no response or progress", idle.as_secs()),
            }
        };
        guard.method.clear();
        if let Some(e) = v.get("error") {
            bail!("{method}: {}", e["message"].as_str().unwrap_or(&e.to_string()));
        }
        Ok(v["result"].clone())
    }

    async fn notify(&self, method: &str) -> Result<()> {
        write(&self.stdin, &json!({ "jsonrpc": "2.0", "method": method })).await
    }

    /// `e` with the tail of the server's stderr, once the server has exited
    /// or half a second has passed.
    async fn with_stderr(&mut self, e: anyhow::Error) -> anyhow::Error {
        if let Some(h) = self.stderr_done.take() {
            let _ = tokio::time::timeout(Duration::from_millis(500), h).await;
        }
        let tail = self.stderr.lock().map(|mut b| String::from_utf8_lossy(b.make_contiguous()).into_owned());
        match tail {
            Ok(t) if !t.trim().is_empty() => anyhow!("{e:#}\nserver stderr:\n{}", t.trim_end()),
            _ => e,
        }
    }
}

/// Undoes a request that did not complete: the pending entry goes, and the
/// server is told to stop (except for `initialize`, which must not be
/// cancelled). An empty `method` means it completed.
struct Guard {
    id: u64,
    method: String,
    state: Arc<State>,
    stdin: Stdin,
}

impl Drop for Guard {
    fn drop(&mut self) {
        self.state.take(self.id);
        if self.method.is_empty() || self.method == "initialize" {
            return;
        }
        let msg = json!({ "jsonrpc": "2.0", "method": "notifications/cancelled", "params": { "requestId": self.id, "reason": "timed out or interrupted" } });
        let stdin = self.stdin.clone();
        if let Ok(h) = tokio::runtime::Handle::try_current() {
            h.spawn(async move {
                let _ = write(&stdin, &msg).await;
            });
        }
    }
}

impl Drop for Conn {
    /// The spec's order: close stdin, give the server a moment to exit, then
    /// SIGTERM. SIGTERM, not SIGKILL, because the sandbox helper sweeps the
    /// server's whole tree (npx, node, …) on SIGTERM and could not if killed.
    /// `stdin` closes when this struct's fields drop, right after this.
    fn drop(&mut self) {
        let Some(mut child) = self.child.take() else { return };
        let term = |c: &Child| {
            if let Some(pid) = c.id() {
                // SAFETY: a plain kill(2) on our own unreaped child.
                unsafe { libc::kill(pid as i32, libc::SIGTERM) };
            }
        };
        match tokio::runtime::Handle::try_current() {
            Ok(h) => {
                h.spawn(async move {
                    if tokio::time::timeout(GRACE, child.wait()).await.is_err() {
                        term(&child);
                        let _ = child.wait().await;
                    }
                });
            }
            Err(_) => term(&child),
        }
    }
}

async fn write(stdin: &Mutex<ChildStdin>, msg: &Value) -> Result<()> {
    let mut w = stdin.lock().await;
    w.write_all(format!("{msg}\n").as_bytes()).await?;
    w.flush().await?;
    Ok(())
}

pub(super) async fn start(cfg: &McpServer, policy: &Policy) -> Result<Conn> {
    let mut cmd = policy.spawn(&cfg.command, &cfg.args);
    cmd.envs(&cfg.env).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    // A named secret that isn't set is left out: many keys are optional
    // (Context7 works without one, with lower rate limits).
    for name in &cfg.secrets {
        if let Some(v) = crate::secrets::get(name) {
            cmd.env(name, v);
        }
    }
    let mut child = cmd.spawn()?;
    let (Some(stdin), Some(stdout), Some(mut stderr)) = (child.stdin.take(), child.stdout.take(), child.stderr.take())
    else {
        bail!("no stdio pipes");
    };
    let stdin: Stdin = Arc::new(Mutex::new(stdin));
    let state = Arc::new(State::default());
    tokio::spawn(read(stdout, Arc::downgrade(&stdin), state.clone()));
    // Drained continuously, or a chatty server would block on a full pipe.
    let tail = Arc::new(std::sync::Mutex::new(VecDeque::with_capacity(STDERR_TAIL)));
    let t = tail.clone();
    let stderr_done = tokio::spawn(async move {
        let mut buf = [0u8; 1024];
        while let Ok(n) = stderr.read(&mut buf).await
            && n > 0
        {
            if let Ok(mut q) = t.lock() {
                q.extend(&buf[..n]);
                let over = q.len().saturating_sub(STDERR_TAIL);
                q.drain(..over);
            }
        }
    });
    let mut conn = Conn {
        stdin,
        state,
        next: AtomicU64::new(1),
        tools: vec![],
        child: Some(child),
        stderr: tail,
        stderr_done: Some(stderr_done),
    };
    match handshake(&conn).await {
        Ok(tools) => {
            conn.tools = tools;
            Ok(conn)
        }
        Err(e) => Err(conn.with_stderr(e).await),
    }
}

/// initialize, check the version, initialized, then every page of tools/list
/// if the server says it has tools.
async fn handshake(conn: &Conn) -> Result<Vec<Value>> {
    let init = conn
        .request(
            "initialize",
            json!({
                "protocolVersion": VERSIONS[0],
                "capabilities": {},
                "clientInfo": { "name": "construct", "version": env!("CARGO_PKG_VERSION") }
            }),
            START_TIMEOUT,
        )
        .await?;
    let v = init["protocolVersion"].as_str().unwrap_or_default();
    ensure!(VERSIONS.contains(&v), "unsupported MCP protocol version {v:?} (construct speaks {})", VERSIONS.join(", "));
    conn.notify("notifications/initialized").await?;
    let mut tools = Vec::new();
    if init["capabilities"].get("tools").is_none() {
        return Ok(tools);
    }
    let mut cursor: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let params = cursor.as_ref().map_or(json!({}), |c| json!({ "cursor": c }));
        let res = conn.request("tools/list", params, START_TIMEOUT).await.context("listing tools")?;
        tools.extend(res["tools"].as_array().cloned().unwrap_or_default());
        let next = res["nextCursor"].as_str().map(String::from);
        if next.is_none() || next == cursor {
            return Ok(tools);
        }
        cursor = next;
    }
    bail!("tools/list did not end after {MAX_PAGES} pages")
}

/// The reader task. Lines are read as bytes, so one that isn't UTF-8 (or
/// isn't JSON) is skipped rather than ending the connection.
async fn read(stdout: tokio::process::ChildStdout, stdin: Weak<Mutex<ChildStdin>>, state: Arc<State>) {
    let mut r = BufReader::new(stdout);
    let mut line = Vec::new();
    loop {
        line.clear();
        match r.read_until(b'\n', &mut line).await {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let Ok(v) = serde_json::from_slice::<Value>(&line) else { continue };
        match (v.get("method").and_then(Value::as_str), v.get("id")) {
            // A request from the server.
            (Some(method), Some(id)) => {
                let reply = if method == "ping" {
                    json!({ "jsonrpc": "2.0", "id": id, "result": {} })
                } else {
                    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": format!("method not found: {method}") } })
                };
                if let Some(stdin) = stdin.upgrade() {
                    let _ = write(&stdin, &reply).await;
                }
            }
            (Some("notifications/progress"), None) => {
                if let Some(id) = v["params"]["progressToken"].as_u64() {
                    state.progress(id);
                }
            }
            // Other notifications (tools/list_changed among them) are not
            // acted on: tools are listed once per start.
            (Some(_), None) => {}
            (None, Some(id)) => {
                if let Some(w) = id.as_u64().and_then(|id| state.take(id)) {
                    let _ = w.tx.send(v);
                }
            }
            (None, None) => {}
        }
    }
    state.close();
}
