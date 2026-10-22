//! `construct-dev mcp-fake`: a stdio MCP server for tests. By default it lists
//! `add` and `echo` and nothing else, so the routed MCP snapshot stays put;
//! flags add the sandbox probes and the spec cases the MCP client must handle.
//!
//! The design choice a reader must know: it is deliberately synchronous and
//! answers one line at a time, so a test controls exactly what the client
//! sees and in what order (a server request mid-call, a garbage line, a crash).

use anyhow::Result;
use serde_json::{Value, json};
use std::io::{BufRead, Write};
use std::time::{Duration, Instant};

#[derive(clap::Args, Debug, Clone, Default)]
pub struct Opts {
    /// Also list the sandbox probes (probe_read, probe_env, probe_connect,
    /// probe_spawn).
    #[arg(long)]
    probes: bool,
    /// Also list the spec cases: odd tool names, crash, server requests, a
    /// garbage line, rich content, progress, a call that never answers.
    #[arg(long)]
    spec: bool,
    /// Answer initialize with this protocol version.
    #[arg(long, default_value = "2025-06-18")]
    version: String,
    /// Declare no tools capability, and refuse tools/list.
    #[arg(long)]
    no_tools: bool,
    /// Write a reason to stderr and exit before reading anything.
    #[arg(long)]
    fail_start: bool,
}

/// A tool name past the 64 characters an API accepts once prefixed.
pub const LONG_NAME: &str = "a_rather_long_tool_name_that_goes_on_and_on_past_what_apis_take_v1";

fn tool(name: &str, schema: Value) -> Value {
    json!({ "name": name, "description": name, "inputSchema": schema })
}

fn tools(o: &Opts) -> Vec<Value> {
    let obj = json!({ "type": "object" });
    let mut t = vec![
        json!({ "name": "add", "description": "Add two integers", "inputSchema": { "type": "object", "properties": { "a": { "type": "integer" }, "b": { "type": "integer" } }, "required": ["a", "b"] } }),
        json!({ "name": "echo", "description": "Echo text back", "inputSchema": { "type": "object", "properties": { "text": { "type": "string" } }, "required": ["text"] } }),
    ];
    if o.probes {
        for p in ["probe_read", "probe_env", "probe_connect", "probe_spawn"] {
            t.push(tool(p, obj.clone()));
        }
    }
    if o.spec {
        t.push(tool("dotted.tool", json!({ "$schema": "http://json-schema.org/draft-07/schema#", "type": "object" })));
        t.push(tool(LONG_NAME, obj.clone()));
        for n in ["crash", "ping_first", "ask_sampling", "garbage", "rich", "structured", "slow", "hang"] {
            t.push(tool(n, obj.clone()));
        }
    }
    t
}

pub fn run(o: Opts) -> Result<()> {
    // Real servers log to stderr; it must never reach the protocol stream.
    eprintln!("fake MCP server running on stdio");
    if o.fail_start {
        eprintln!("fake: cannot open config /nonexistent/fake.toml");
        std::process::exit(2);
    }
    let stdin = std::io::stdin();
    let mut lines = stdin.lock().lines();
    let mut out = std::io::stdout();
    let mut send = |v: Value| -> Result<()> {
        writeln!(out, "{v}")?;
        Ok(out.flush()?)
    };
    while let Some(line) = lines.next() {
        let Ok(v) = serde_json::from_str::<Value>(&line?) else { continue };
        let method = v["method"].as_str().unwrap_or("");
        let Some(id) = v.get("id").cloned() else {
            if method == "notifications/cancelled" {
                let mut log = std::fs::OpenOptions::new().create(true).append(true).open("mcp-fake.log")?;
                writeln!(log, "cancelled {}", v["params"]["requestId"])?;
            }
            continue;
        };
        let result = match method {
            "initialize" => {
                let caps = if o.no_tools { json!({}) } else { json!({ "tools": {} }) };
                json!({ "protocolVersion": o.version, "capabilities": caps, "serverInfo": { "name": "fake", "version": "1" } })
            }
            "tools/list" if o.no_tools => {
                send(json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": "no tools here" } }))?;
                continue;
            }
            "tools/list" => json!({ "tools": tools(&o) }),
            "tools/call" => {
                let a = &v["params"]["arguments"];
                let name = v["params"]["name"].as_str().unwrap_or("");
                let mut ask = |req: Value| -> Result<Value> {
                    send(req)?;
                    Ok(lines.next().transpose()?.and_then(|l| serde_json::from_str(&l).ok()).unwrap_or_default())
                };
                let text = match name {
                    "add" => (a["a"].as_i64().unwrap_or(0) + a["b"].as_i64().unwrap_or(0)).to_string(),
                    "echo" => a["text"].as_str().unwrap_or("").to_string(),
                    "probe_read" => match std::fs::read_to_string(a["path"].as_str().unwrap_or("")) {
                        Ok(t) => format!("READ {t}"),
                        Err(e) => format!("DENIED {e}"),
                    },
                    "probe_env" => std::env::vars().map(|(k, v)| format!("{k}={v}\n")).collect(),
                    "probe_connect" => match std::net::TcpStream::connect(a["addr"].as_str().unwrap_or("")) {
                        Ok(_) => "CONNECTED".into(),
                        Err(e) => format!("DENIED {e}"),
                    },
                    "probe_spawn" => std::process::Command::new("sh")
                        .args(["-c", "setsid sleep 300 >/dev/null 2>&1 & echo $!"])
                        .output()
                        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                        .unwrap_or_else(|e| format!("DENIED {e}")),
                    "dotted.tool" => "dotted ok".into(),
                    LONG_NAME => "long ok".into(),
                    "crash" => std::process::exit(3),
                    "ping_first" => {
                        let r = ask(json!({ "jsonrpc": "2.0", "id": "srv-1", "method": "ping" }))?;
                        if r["id"] == "srv-1" && r["result"] == json!({}) {
                            "pong ok".into()
                        } else {
                            format!("pong bad: {r}")
                        }
                    }
                    "ask_sampling" => {
                        let r = ask(
                            json!({ "jsonrpc": "2.0", "id": "srv-2", "method": "sampling/createMessage", "params": {} }),
                        )?;
                        if r["id"] == "srv-2" && r["error"]["code"] == -32601 {
                            "refused ok".into()
                        } else {
                            format!("refused bad: {r}")
                        }
                    }
                    "garbage" => {
                        std::io::stdout().write_all(b"\xff\xfe not json\n")?;
                        "survived".into()
                    }
                    "rich" => {
                        send(json!({ "jsonrpc": "2.0", "id": id, "result": { "content": [
                            { "type": "text", "text": "intro" },
                            { "type": "resource", "resource": { "uri": "file:///notes.md", "text": "embedded notes" } },
                            { "type": "resource_link", "uri": "file:///src/parser.rs", "name": "parser.rs" }
                        ] } }))?;
                        continue;
                    }
                    "structured" => {
                        send(
                            json!({ "jsonrpc": "2.0", "id": id, "result": { "content": [], "structuredContent": { "answer": 42 } } }),
                        )?;
                        continue;
                    }
                    "slow" => {
                        let token = v["params"]["_meta"]["progressToken"].clone();
                        let until = Instant::now() + Duration::from_millis(a["ms"].as_u64().unwrap_or(1000));
                        let mut n = 0;
                        while Instant::now() < until {
                            std::thread::sleep(Duration::from_millis(200));
                            n += 1;
                            send(
                                json!({ "jsonrpc": "2.0", "method": "notifications/progress", "params": { "progressToken": token, "progress": n } }),
                            )?;
                        }
                        "slow done".into()
                    }
                    "hang" => continue,
                    _ => "unknown tool".into(),
                };
                json!({ "content": [{ "type": "text", "text": text }] })
            }
            _ => json!({}),
        };
        send(json!({ "jsonrpc": "2.0", "id": id, "result": result }))?;
    }
    Ok(())
}
