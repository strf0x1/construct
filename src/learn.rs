//! The learning pass. A review agent reads a digest of the session and may
//! only touch memory and skills. It runs in the background: as a task while
//! the TUI is open, and as a detached `construct learn` process when a
//! session ends, so quitting never waits on it.
//!
//! The review prompts adapt Hermes's (agent/background_review.py): embed
//! corrections in the skill that governs the task, prefer patching an
//! existing class-level skill to minting a narrow new one, and never encode
//! transient environment failures as rules.

use crate::config::{Config, Paths};
use crate::llm::{self, Message};
use crate::memory::Store;
use crate::tools::{self, Ctx, Group};
use anyhow::Result;
use serde_json::{Value, json};
use std::fmt::Write as _;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Memory,
    Skills,
    Both,
}

const SYSTEM: &str = "You are construct's learning pass. You read a finished stretch of an agent session and improve two durable stores so the next session goes better. You can only use the memory, skill and skill_manage tools. Do the work in as few calls as you can, then reply with one short line saying what changed (or 'Nothing to save.').";

const MEMORY_FOCUS: &str = "MEMORY — who the user is and what their machine is like.
- Save what stops the user repeating themselves: stated preferences, corrections to how you work, their role and projects, stable facts about the machine (OS quirks, toolchains, paths, services).
- Use target 'user' for the person, 'memory' for the machine and projects. Both are capped: batch your changes in one call and remove or merge stale entries to make room.
- Skip task progress, one-off details, anything easy to rediscover, and secrets.";

const SKILL_FOCUS: &str = "SKILLS — how to do a class of task for this user.
Act when any of these happened:
- the user corrected your approach, style or output (frustration is a first-class signal);
- a non-trivial technique, fix, workaround or debugging path emerged;
- a skill that was loaded turned out wrong, incomplete or outdated.
Preference order, take the first that fits:
1. patch a skill that was loaded this session;
2. patch an existing class-level skill that covers the territory (skill_manage list, then skill to read it);
3. add a support file (references/, templates/ or scripts/) under an existing skill and point to it from SKILL.md;
4. create a new class-level skill. Name it for the class of work, never for today's task, a PR, an error string or a single library.
A good SKILL.md: frontmatter (name, description saying when to use it), then trigger, numbered steps with exact commands, pitfalls, and how to verify.
Never capture: environment failures the user can fix (missing binaries, unset credentials), negative claims about tools ('X doesn't work'), errors that a retry fixed, or one-off task narratives.";

const NOTHING: &str = "If nothing in the session meets the bar, reply 'Nothing to save.' and stop. Don't reach for that by default.";

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

/// Run one review over `messages`. Returns what changed.
pub async fn review(cfg: &Config, paths: &Paths, messages: &[Message], kind: Kind) -> Result<Outcome> {
    let key = crate::secrets::api_key(&cfg.api_key_secret);
    let client = llm::Client::new(&cfg.base_url, key, cfg.headers.clone())?;
    let memory = Store::new(paths, &cfg.memory);
    let (_tx, rx) = tokio::sync::watch::channel(false);
    let ctx = Arc::new(Ctx::new(std::env::current_dir().unwrap_or_default(), paths.clone(), memory, rx, None));

    let lib = ctx.skills.list();
    let mut task = String::new();
    let _ = writeln!(task, "<session>\n{}\n</session>\n", digest(messages, 60_000));
    let _ = writeln!(task, "Current memory:{}", if ctx.memory.prompt_block().is_empty() { " (empty)".to_string() } else { ctx.memory.prompt_block() });
    if lib.is_empty() {
        let _ = writeln!(task, "\nSkill library: empty.");
    } else {
        let _ = writeln!(task, "\nSkill library:");
        for s in &lib {
            let _ = writeln!(task, "- {}: {}", s.name, s.description);
        }
    }
    let _ = writeln!(task);
    if matches!(kind, Kind::Memory | Kind::Both) {
        let _ = writeln!(task, "{MEMORY_FOCUS}\n");
    }
    if matches!(kind, Kind::Skills | Kind::Both) {
        let _ = writeln!(task, "{SKILL_FOCUS}\n");
    }
    let _ = writeln!(task, "{NOTHING}");

    let mut groups = vec![];
    if matches!(kind, Kind::Memory | Kind::Both) {
        groups.push(Group::Memory);
    }
    if matches!(kind, Kind::Skills | Kind::Both) {
        groups.extend([Group::Skills, Group::SkillWrite]);
    }
    let defs = tools::definitions(&groups, &ctx);
    run_loop(&client, cfg.learn_model(), &ctx, vec![Message::system(SYSTEM), Message::user(task)], &defs, 10).await
}

async fn run_loop(client: &llm::Client, model: &str, ctx: &Ctx, mut msgs: Vec<Message>, defs: &[Value], max: u32) -> Result<Outcome> {
    let allowed: Vec<String> = defs.iter().filter_map(|d| d.pointer("/function/name").and_then(Value::as_str).map(String::from)).collect();
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
                tools::call(ctx, &call.function.name, &call.function.arguments).await
            } else {
                Err(format!("{} isn't available in the learning pass", call.function.name))
            };
            let text = match &out {
                Ok(s) => s.clone(),
                Err(e) => format!("Error: {e}"),
            };
            if out.is_ok() && call.function.name != "skill" && !(call.function.name == "skill_manage" && call.function.arguments.contains("\"list\"")) {
                actions.push(format!("{}: {}", call.function.name, llm::truncate(&text, 120)));
            }
            msgs.push(Message::tool(&call.id, text));
        }
    }
    Ok(Outcome { summary: "stopped at the step limit".into(), actions })
}

const CURATOR: &str = "You maintain an agent-written skill library. Below is every agent-written skill with usage stats. Improve the library's shape:
- merge skills that overlap into one class-level umbrella (patch or edit the umbrella first, then delete the others with absorbed_into);
- archive skills that are stale (unused for a long time) and narrow, with delete and absorbed_into \"\";
- tighten descriptions so it is obvious when each skill applies.
Never touch pinned skills. Deleting only archives, but be conservative: when unsure, leave a skill alone. Reply with one line summarizing what you changed.";

pub async fn curate(cfg: &Config, paths: &Paths) -> Result<Option<Outcome>> {
    let key = crate::secrets::api_key(&cfg.api_key_secret);
    let client = llm::Client::new(&cfg.base_url, key, cfg.headers.clone())?;
    let memory = Store::new(paths, &cfg.memory);
    let (_tx, rx) = tokio::sync::watch::channel(false);
    let ctx = Ctx::new(std::env::current_dir().unwrap_or_default(), paths.clone(), memory, rx, None);
    let usage = ctx.skills.usage();
    let now = crate::skills::now();
    let mut listing = String::new();
    let mut n = 0;
    for s in ctx.skills.list() {
        let u = usage.get(&s.name).cloned().unwrap_or_default();
        if u.author != "agent" || u.pinned {
            continue;
        }
        n += 1;
        let idle = if u.last_used == 0 { "never".to_string() } else { format!("{}d ago", (now.saturating_sub(u.last_used)) / 86_400) };
        let _ = writeln!(listing, "- {} (views {}, patches {}, last used {idle}): {}", s.name, u.views, u.patches, s.description);
    }
    if n < 3 {
        return Ok(None);
    }
    let defs = tools::definitions(&[Group::Skills, Group::SkillWrite], &ctx);
    let out = run_loop(&client, cfg.learn_model(), &ctx, vec![Message::system(CURATOR), Message::user(listing)], &defs, 16).await?;
    Ok(Some(out))
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
