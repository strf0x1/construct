use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use construct_dev::{drive, serve};
use std::io::Read;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "construct-dev", about = "Drive and observe construct (and other harnesses) end to end")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Serve an OpenAI-compatible endpoint: scripted, or a recording proxy.
    Serve {
        #[arg(long, default_value_t = 0)]
        port: u16,
        /// JSON file with {"steps": [...]} (see serve.rs).
        #[arg(long)]
        script: Option<PathBuf>,
        /// After the script, answer with this call N times: '{"call":"read","args":{"path":"x"}}' N
        #[arg(long, num_args = 2, value_names = ["STEP", "N"])]
        repeat: Option<Vec<String>>,
        /// Forward to this base URL (e.g. https://openrouter.ai/api/v1).
        #[arg(long)]
        upstream: Option<String>,
        /// Env var holding the upstream key.
        #[arg(long, default_value = "OPENROUTER_API_KEY")]
        key_env: String,
        #[arg(long)]
        log: Option<PathBuf>,
        /// Save every request body here.
        #[arg(long)]
        dump: Option<PathBuf>,
        #[arg(long, default_value_t = 2)]
        chunk_ms: u64,
        /// Write the bound port to this file once listening.
        #[arg(long)]
        port_file: Option<PathBuf>,
    },
    /// A tiny stdio MCP server (tools: add, echo) for tests.
    McpFake,
    /// Run a program in a pty and execute a drive script against it.
    Drive {
        /// Script file, or - for stdin.
        #[arg(long, default_value = "-")]
        script: String,
        #[arg(long, default_value_t = 120)]
        cols: u16,
        #[arg(long, default_value_t = 36)]
        rows: u16,
        /// KEY=VALUE environment for the program.
        #[arg(long = "env")]
        env: Vec<String>,
        #[arg(long)]
        cwd: Option<String>,
        /// Print timing metrics as JSON at the end.
        #[arg(long)]
        metrics: bool,
        #[arg(trailing_var_arg = true, required = true)]
        cmd: Vec<String>,
    },
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Serve { port, script, repeat, upstream, key_env, log, dump, chunk_ms, port_file } => {
            let steps = match script {
                Some(p) => {
                    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&p)?)?;
                    serde_json::from_value(v["steps"].clone()).context("parsing steps")?
                }
                None => vec![],
            };
            let repeat_call = match repeat {
                Some(v) => Some((serde_json::from_str(&v[0]).context("parsing repeat step")?, v[1].parse()?)),
                None => None,
            };
            let key = if upstream.is_some() { std::env::var(&key_env).ok() } else { None };
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(async move {
                let cfg = serve::Config { script: steps, repeat_call, upstream, key, log, dump_dir: dump, chunk_ms };
                let (port, h) = serve::serve(cfg, port).await?;
                if let Some(f) = port_file {
                    std::fs::write(f, port.to_string())?;
                }
                eprintln!("listening on http://127.0.0.1:{port}/v1");
                let _ = h.await;
                Ok(())
            })
        }
        Cmd::McpFake => mcp_fake(),
        Cmd::Drive { script, cols, rows, env, cwd, metrics, cmd } => {
            let text = if script == "-" {
                let mut s = String::new();
                std::io::stdin().read_to_string(&mut s)?;
                s
            } else {
                std::fs::read_to_string(&script)?
            };
            let env: Vec<(String, String)> = env
                .iter()
                .filter_map(|kv| kv.split_once('=').map(|(k, v)| (k.to_string(), v.to_string())))
                .collect();
            let mut d = drive::Driver::spawn(&cmd, &env, cwd.as_deref(), cols, rows)?;
            let mut out = std::io::stdout();
            let r = drive::run_script(&mut d, &text, &mut out);
            d.kill();
            match r {
                Ok(m) => {
                    if metrics {
                        println!("{m}");
                    }
                    Ok(())
                }
                Err(e) => {
                    eprintln!("{e:#}");
                    std::process::exit(1);
                }
            }
        }
    }
}

fn mcp_fake() -> Result<()> {
    use serde_json::{Value, json};
    use std::io::{BufRead, Write};
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = line?;
        let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
        let Some(id) = v.get("id").cloned() else { continue };
        let result = match v["method"].as_str().unwrap_or("") {
            "initialize" => json!({ "protocolVersion": "2025-06-18", "capabilities": { "tools": {} }, "serverInfo": { "name": "fake", "version": "1" } }),
            "tools/list" => json!({ "tools": [
                { "name": "add", "description": "Add two integers", "inputSchema": { "type": "object", "properties": { "a": { "type": "integer" }, "b": { "type": "integer" } }, "required": ["a", "b"] } },
                { "name": "echo", "description": "Echo text back", "inputSchema": { "type": "object", "properties": { "text": { "type": "string" } }, "required": ["text"] } }
            ] }),
            "tools/call" => {
                let a = &v["params"]["arguments"];
                let text = match v["params"]["name"].as_str() {
                    Some("add") => (a["a"].as_i64().unwrap_or(0) + a["b"].as_i64().unwrap_or(0)).to_string(),
                    Some("echo") => a["text"].as_str().unwrap_or("").to_string(),
                    _ => "unknown tool".into(),
                };
                json!({ "content": [{ "type": "text", "text": text }] })
            }
            _ => json!({}),
        };
        writeln!(out, "{}", json!({ "jsonrpc": "2.0", "id": id, "result": result }))?;
        out.flush()?;
    }
    Ok(())
}
