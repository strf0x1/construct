//! A minimal MCP client over stdio: initialize, tools/list, tools/call, with
//! newline-delimited JSON-RPC. Servers start lazily the first time the router
//! switches their group on, so an unused server costs nothing at startup.
//!
//! The design choice a reader must know: a server is a model-chosen action
//! like a shell command, so it runs the same way, under `construct __sandbox`
//! with the workspace policy plus what its entry adds, a scrubbed environment
//! plus the secrets it names, and the helper sweeping whatever it started.

use crate::config::{McpServer, Paths, SandboxConfig};
use crate::sandbox::Policy;
use anyhow::{Context, Result, anyhow};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin};
use tokio::sync::{Mutex, oneshot};

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>>;

struct Conn {
    stdin: Mutex<ChildStdin>,
    pending: Pending,
    next: AtomicU64,
    tools: Vec<Value>,
    child: Child,
}

impl Drop for Conn {
    /// SIGTERM, not SIGKILL: the sandbox helper sweeps the server's whole
    /// tree (npx, node, …) on SIGTERM, and could not if it were killed.
    fn drop(&mut self) {
        if let Some(pid) = self.child.id() {
            // SAFETY: a plain kill(2) on our own unreaped child.
            unsafe { libc::kill(pid as i32, libc::SIGTERM) };
        }
    }
}

/// The wire name of a server's tool: `mcp__<server>__<tool>`. Declared here
/// and nowhere else; `server_of` is its inverse.
pub fn wire_name(server: &str, tool: &str) -> String {
    format!("mcp__{server}__{tool}")
}

/// The server a wire name belongs to, if it is an MCP tool's.
pub fn server_of(name: &str) -> Option<&str> {
    name.strip_prefix("mcp__")?.split_once("__").map(|(s, _)| s)
}

/// One tool a server listed, as `tools/list` described it.
pub struct RemoteTool {
    pub name: String,
    pub description: String,
    pub schema: Value,
}

pub struct Manager {
    servers: BTreeMap<String, McpServer>,
    policies: BTreeMap<String, Policy>,
    conns: Mutex<HashMap<String, Arc<Conn>>>,
}

impl Manager {
    pub fn new(servers: BTreeMap<String, McpServer>, sandbox: &SandboxConfig, cwd: &Path, paths: &Paths) -> Self {
        let policies = servers.iter().map(|(n, s)| (n.clone(), Policy::for_mcp(sandbox, s, cwd, paths))).collect();
        Self { servers, policies, conns: Mutex::new(HashMap::new()) }
    }

    pub fn servers(&self) -> &BTreeMap<String, McpServer> {
        &self.servers
    }

    /// Start a server if needed; the tools it listed.
    pub async fn ensure(&self, name: &str) -> Result<Vec<RemoteTool>> {
        let conn = self.conn(name).await?;
        Ok(conn
            .tools
            .iter()
            .map(|t| RemoteTool {
                name: t["name"].as_str().unwrap_or("").to_string(),
                description: t["description"].as_str().unwrap_or("").to_string(),
                schema: t.get("inputSchema").cloned().unwrap_or(json!({"type":"object"})),
            })
            .collect())
    }

    async fn conn(&self, name: &str) -> Result<Arc<Conn>> {
        let mut conns = self.conns.lock().await;
        if let Some(c) = conns.get(name) {
            return Ok(c.clone());
        }
        let cfg = self.servers.get(name).ok_or_else(|| anyhow!("no MCP server {name}"))?;
        let policy = &self.policies[name];
        let conn = Arc::new(start(cfg, policy).await.with_context(|| format!("starting MCP server {name}"))?);
        conns.insert(name.to_string(), conn.clone());
        Ok(conn)
    }

    pub async fn call(&self, server: &str, tool: &str, args: Value) -> Result<String, String> {
        let conn = self.conn(server).await.map_err(|e| e.to_string())?;
        let res = request(&conn, "tools/call", json!({ "name": tool, "arguments": args }), 300)
            .await
            .map_err(|e| e.to_string())?;
        let text = res["content"]
            .as_array()
            .map(|c| {
                c.iter()
                    .map(|b| match b["type"].as_str() {
                        Some("text") => b["text"].as_str().unwrap_or("").to_string(),
                        Some(t) => format!("[{t} content]"),
                        None => b.to_string(),
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_else(|| res.to_string());
        if res["isError"].as_bool().unwrap_or(false) { Err(text) } else { Ok(text) }
    }
}

async fn start(cfg: &McpServer, policy: &Policy) -> Result<Conn> {
    let mut cmd = policy.spawn(&cfg.command, &cfg.args);
    cmd.envs(&cfg.env).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
    // A named secret that isn't set is left out: many keys are optional
    // (Context7 works without one, with lower rate limits).
    for name in &cfg.secrets {
        if let Some(v) = crate::secrets::get(name) {
            cmd.env(name, v);
        }
    }
    let mut child = cmd.spawn()?;
    let stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
    let p = pending.clone();
    tokio::spawn(async move {
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
            if let Some(id) = v.get("id").and_then(Value::as_u64)
                && v.get("method").is_none()
                && let Some(tx) = p.lock().await.remove(&id)
            {
                let _ = tx.send(v);
            }
        }
    });
    let mut conn = Conn { stdin: Mutex::new(stdin), pending, next: AtomicU64::new(1), tools: vec![], child };
    request(
        &conn,
        "initialize",
        json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": { "name": "construct", "version": env!("CARGO_PKG_VERSION") }
        }),
        30,
    )
    .await?;
    notify(&conn, "notifications/initialized").await?;
    let mut tools = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let params = match &cursor {
            Some(c) => json!({ "cursor": c }),
            None => json!({}),
        };
        let res = request(&conn, "tools/list", params, 30).await?;
        tools.extend(res["tools"].as_array().cloned().unwrap_or_default());
        cursor = res["nextCursor"].as_str().map(String::from);
        if cursor.is_none() {
            break;
        }
    }
    conn.tools = tools;
    Ok(conn)
}

async fn request(conn: &Conn, method: &str, params: Value, timeout_s: u64) -> Result<Value> {
    let id = conn.next.fetch_add(1, Ordering::Relaxed);
    let (tx, rx) = oneshot::channel();
    conn.pending.lock().await.insert(id, tx);
    let msg = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
    {
        let mut w = conn.stdin.lock().await;
        w.write_all(format!("{msg}\n").as_bytes()).await?;
        w.flush().await?;
    }
    let v = tokio::time::timeout(Duration::from_secs(timeout_s), rx)
        .await
        .map_err(|_| anyhow!("{method} timed out"))?
        .map_err(|_| anyhow!("MCP server closed"))?;
    if let Some(e) = v.get("error") {
        return Err(anyhow!("{method}: {}", e["message"].as_str().unwrap_or(&e.to_string())));
    }
    Ok(v["result"].clone())
}

async fn notify(conn: &Conn, method: &str) -> Result<()> {
    let msg = json!({ "jsonrpc": "2.0", "method": method });
    let mut w = conn.stdin.lock().await;
    w.write_all(format!("{msg}\n").as_bytes()).await?;
    w.flush().await?;
    Ok(())
}
