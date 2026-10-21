//! The learning pass. A review agent reads a digest of the session and may
//! only touch memory and skills. It runs in the background: as a task while
//! the TUI is open, and as a detached `construct learn` process when a
//! session ends, so quitting never waits on it.
//!
//! The review prompts adapt Hermes's (agent/background_review.py): embed
//! corrections in the skill that governs the task, prefer patching an
//! existing class-level skill to minting a narrow new one, and never encode
//! transient environment failures as rules.

mod curator;
mod gate;
mod ledger;
mod prompts;
mod review;

pub use curator::curate;
pub use gate::{Gate, gate, gate_parts};
pub use ledger::{Observation, ledger};
pub use review::review;

use crate::config::{Config, Paths};
use crate::llm::{self, Message};
use crate::memory::Store;
use crate::tools::{Catalog, Ctx, ToolError};
use anyhow::Result;
use serde_json::{Value, json};
use std::fmt::Write as _;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Memory,
    Skills,
    Both,
}

/// Render messages as compact text: tool outputs clipped, newest kept when
/// the whole thing exceeds `max_chars`.
pub fn digest(messages: &[Message], max_chars: usize) -> String {
    let mut parts: Vec<String> = Vec::new();
    for m in messages {
        let mut s = String::new();
        match m.role.as_str() {
            "system" => continue,
            "user" => {
                let _ = write!(s, "USER: {}", m.content.as_deref().unwrap_or(""));
            }
            "assistant" => {
                if let Some(c) = &m.content {
                    let _ = write!(s, "ASSISTANT: {c}");
                }
                for c in &m.tool_calls {
                    let _ = write!(s, "\nTOOL CALL {}: {}", c.function.name, llm::truncate(&c.function.arguments, 600));
                }
            }
            "tool" => {
                let _ = write!(s, "TOOL RESULT: {}", llm::truncate(m.content.as_deref().unwrap_or(""), 400));
            }
            _ => continue,
        }
        parts.push(s);
    }
    let mut total = 0;
    let mut keep = Vec::new();
    for p in parts.iter().rev() {
        total += p.len() + 2;
        if total > max_chars {
            keep.push("[… earlier conversation omitted …]".to_string());
            break;
        }
        keep.push(p.clone());
    }
    keep.reverse();
    keep.join("\n\n")
}

pub struct Outcome {
    pub summary: String,
    pub actions: Vec<String>,
}

/// The learner only gets memory and skill tools, but its context still
/// carries a sandbox: read-only, so nothing it does can write the workspace.
/// Its catalog is built from the same `[tools]` config as the session's, so a
/// disabled tool doesn't exist here either.
fn learner(
    cfg: &Config,
    paths: &Paths,
    memory: Store,
    rx: tokio::sync::watch::Receiver<bool>,
) -> Result<(Ctx, Catalog)> {
    let cwd = std::env::current_dir().unwrap_or_default();
    let memory = memory.with_project(paths, &cwd);
    let mut sb = cfg.sandbox.clone();
    if sb.mode != "off" {
        sb.mode = "read-only".into();
    }
    let policy = crate::sandbox::Policy::new(&sb, &cwd, paths);
    Ok((Ctx::new(cwd, paths.clone(), memory, rx, None, policy), Catalog::build(&cfg.tools, None)?))
}

async fn run_loop(
    client: &llm::Client,
    model: &str,
    ctx: &Ctx,
    catalog: &Catalog,
    mut msgs: Vec<Message>,
    defs: &[Value],
    max: u32,
) -> Result<Outcome> {
    let allowed: Vec<String> =
        defs.iter().filter_map(|d| d.pointer("/function/name").and_then(Value::as_str).map(String::from)).collect();
    let mut actions = Vec::new();
    for _ in 0..max {
        let c = client
            .complete(llm::Request { model, messages: &msgs, tools: defs, max_tokens: Some(4000), temperature: None })
            .await?;
        let m = c.message;
        msgs.push(m.clone());
        if m.tool_calls.is_empty() {
            return Ok(Outcome { summary: m.content.unwrap_or_default().trim().to_string(), actions });
        }
        for call in &m.tool_calls {
            let out = if allowed.contains(&call.function.name) {
                catalog.call(ctx, &call.function.name, &call.function.arguments).await
            } else {
                Err(ToolError::from(format!("{} isn't available in the learning pass", call.function.name)))
            };
            let text = match &out {
                Ok(o) => o.text.clone(),
                Err(e) => format!("Error: {e}"),
            };
            // Loading a skill or listing the library changes nothing; it isn't an action.
            if out.as_ref().is_ok_and(|o| o.changed()) && !catalog.readonly(&call.function.name) {
                actions.push(format!("{}: {}", call.function.name, llm::truncate(&text, 120)));
            }
            msgs.push(Message::tool(&call.id, text));
        }
    }
    Ok(Outcome { summary: "stopped at the step limit".into(), actions })
}

/// Small persisted state: when the curator last ran, which sessions were reviewed.
pub fn state(paths: &Paths) -> Value {
    std::fs::read_to_string(paths.state_file()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(json!({}))
}

pub fn save_state(paths: &Paths, v: &Value) {
    let _ = crate::config::write_atomic(&paths.state_file(), v.to_string().as_bytes());
}

pub fn log_line(paths: &Paths, line: &str) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(paths.learn_log()) {
        let _ = writeln!(f, "{} {line}", crate::session::now_ms());
    }
}

/// Start `construct learn --session <id>` detached, so it outlives us.
pub fn spawn_detached(session_id: &str) {
    use std::os::unix::process::CommandExt;
    let Ok(exe) = std::env::current_exe() else { return };
    let _ = std::process::Command::new(exe)
        .args(["learn", "--session", session_id])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0)
        .spawn();
}

/// What a finished session deserves: nothing for a trivial exchange.
pub fn kind_for(user_turns: u32, tool_calls: u32) -> Option<Kind> {
    match (user_turns, tool_calls) {
        (0, _) => None,
        (1, t) if t < 3 => Some(Kind::Memory),
        (_, t) if t >= 3 => Some(Kind::Both),
        _ => Some(Kind::Memory),
    }
}

fn log_line_quiet(line: &str) {
    if let Ok(p) = Paths::resolve() {
        log_line(&p, line);
    }
}
