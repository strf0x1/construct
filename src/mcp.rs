//! A minimal MCP client over stdio: initialize, tools/list, tools/call, with
//! newline-delimited JSON-RPC. Servers start lazily the first time the router
//! switches their group on, so an unused server costs nothing at startup.

use crate::config::McpServer;
use anyhow::{Context, Result, anyhow};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};
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
    _child: Child,
}

pub struct Manager {
    servers: BTreeMap<String, McpServer>,
    conns: Mutex<HashMap<String, Arc<Conn>>>,
    /// Tool definitions per started server, readable without awaiting.
    defs: std::sync::Mutex<HashMap<String, Vec<Value>>>,
}

impl Manager {
    pub fn new(servers: BTreeMap<String, McpServer>) -> Self {
        Self { servers, conns: Mutex::new(HashMap::new()), defs: std::sync::Mutex::new(HashMap::new()) }
    }

    pub fn servers(&self) -> &BTreeMap<String, McpServer> {
        &self.servers
    }

    /// Start a server if needed and cache its tool definitions.
    pub async fn ensure(&self, name: &str) -> Result<()> {
        let mut conns = self.conns.lock().await;
        if conns.contains_key(name) {
            return Ok(());
        }
        let cfg = self.servers.get(name).ok_or_else(|| anyhow!("no MCP server {name}"))?;
        let conn = Arc::new(start(cfg).await.with_context(|| format!("starting MCP server {name}"))?);
        let defs = conn
            .tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "function": {
                        "name": format!("mcp__{name}__{}", t["name"].as_str().unwrap_or("")),
                        "description": crate::llm::truncate(t["description"].as_str().unwrap_or(""), 1024),
                        "parameters": t.get("inputSchema").cloned().unwrap_or(json!({"type":"object"})),
                    }
                })
            })
            .collect();
        self.defs.lock().unwrap().insert(name.to_string(), defs);
        conns.insert(name.to_string(), conn);
        Ok(())
    }

    pub fn definitions(&self, server: &str) -> Vec<Value> {
        self.defs.lock().unwrap().get(server).cloned().unwrap_or_default()
    }

    pub async fn call(&self, full: &str, args: Value) -> Result<String, String> {
        let rest = full.strip_prefix("mcp__").ok_or("not an MCP tool")?;
        let (server, tool) = rest.split_once("__").ok_or("malformed MCP tool name")?;
        self.ensure(server).await.map_err(|e| e.to_string())?;
        let conn = self.conns.lock().await.get(server).cloned().ok_or("server not running")?;
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

async fn start(cfg: &McpServer) -> Result<Conn> {
    let mut cmd = tokio::process::Command::new(&cfg.command);
    cmd.args(&cfg.args)
        .envs(&cfg.env)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let mut child = cmd.spawn()?;
    let stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
    let p = pending.clone();
    tokio::spawn(async move {
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
            if let Some(id) = v.get("id").and_then(Value::as_u64) {
                if v.get("method").is_none() {
                    if let Some(tx) = p.lock().await.remove(&id) {
                        let _ = tx.send(v);
                    }
                }
            }
        }
    });
    let mut conn = Conn { stdin: Mutex::new(stdin), pending, next: AtomicU64::new(1), tools: vec![], _child: child };
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
