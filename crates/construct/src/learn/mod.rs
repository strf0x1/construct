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
mod evidence;
mod gate;
mod hybrid;
mod ledger;
pub mod playbook;
mod prompts;
mod review;
pub mod workflow;

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

/// `[learn] strategy` values.
pub const SKILLS: &str = "skills";
pub const CORRECTIONS: &str = "corrections";
pub const PLAYBOOK: &str = "playbook";
pub const WORKFLOW: &str = "workflow";
pub const HYBRID: &str = "hybrid";

/// For the strategies that replace the review (`playbook`, `workflow`,
/// `hybrid`): learn from `messages` with one call. `hybrid` asks
/// `gate::evidence` first, so a session with nothing in it costs no call;
/// the other two reflect every time. `None` for the strategies that go
/// through `gate` and `review`.
pub async fn distill(cfg: &Config, paths: &Paths, session: &str, messages: &[Message]) -> Option<Result<Outcome>> {
    match cfg.learn.strategy.as_str() {
        PLAYBOOK => Some(playbook::reflect(cfg, paths, messages).await),
        WORKFLOW => Some(workflow::induce(cfg, paths, messages).await),
        HYBRID => {
            let g = gate::evidence(cfg, messages).await;
            log_line(paths, &format!("session {session}: gate {} {} -> {:?}", g.source, g.note, g.kind));
            if g.kind.is_none() {
                return Some(Ok(Outcome { summary: "Nothing to learn.".into(), actions: vec![] }));
            }
            Some(hybrid::reflect(cfg, paths, messages).await)
        }
        _ => None,
    }
}

/// What the configured strategy adds to the system prompt. Only `playbook`,
/// `hybrid` (the same playbook; its skills are in the project skill index)
/// and `workflow` keep a store of their own; the others add nothing and
/// touch no files.
pub fn prompt_block(strategy: &str, memory: &Store) -> String {
    let kit = memory.project.as_ref().map(|p| p.dir.as_path());
    match strategy {
        PLAYBOOK | HYBRID => playbook::prompt_block(memory.dir(), kit),
        WORKFLOW => workflow::prompt_block(kit),
        _ => String::new(),
    }
}

/// One completion that must answer with a JSON object; anything around the
/// object is ignored, and no object at all reads as `{}`.
async fn ask_json(client: &llm::Client, model: &str, system: &str, user: &str, max_tokens: u32) -> Result<Value> {
    let msgs = vec![Message::system(system), Message::user(user)];
    let c = client
        .complete(llm::Request { model, messages: &msgs, tools: &[], max_tokens: Some(max_tokens), temperature: None })
        .await?;
    let text = c.message.content.unwrap_or_default();
    Ok(serde_json::from_str(json_object(&text)).unwrap_or(json!({})))
}

/// The outermost `{...}` of a model's reply, which may wrap it in prose or a
/// fence; `{}` when there is none.
fn json_object(text: &str) -> &str {
    text.find('{').and_then(|a| text.get(a..=text.rfind('}')?)).unwrap_or("{}")
}

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
        let _ = writeln!(f, "{} {line}", crate::journal::now_ms());
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_object_survives_a_brace_before_the_object() {
        assert_eq!(json_object("sure: {\"a\": 1} done"), "{\"a\": 1}");
        assert_eq!(json_object("} then {"), "{}");
        assert_eq!(json_object("no json, é"), "{}");
    }

    #[test]
    fn only_the_playbook_strategy_reads_a_playbook() {
        let dir = std::env::temp_dir().join(format!("construct-learn-prompt-{}", std::process::id()));
        let paths = Paths { home: dir.clone() };
        let memory = Store::new(&paths, &crate::config::MemoryConfig::default());
        let bullets = vec![playbook::Bullet { id: "g1".into(), text: "Run ./tools/test".into(), ..Default::default() }];
        std::fs::create_dir_all(memory.dir()).unwrap();
        std::fs::write(playbook::global_path(memory.dir()), serde_json::to_string(&bullets).unwrap()).unwrap();
        assert!(prompt_block(PLAYBOOK, &memory).contains("[g1] Run ./tools/test"));
        for other in [SKILLS, CORRECTIONS, WORKFLOW] {
            assert_eq!(prompt_block(other, &memory), "", "{other} must not show a playbook");
        }
        let _ = std::fs::remove_dir_all(dir);
    }
}
