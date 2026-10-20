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
- Save only what the user said about how they want you to work (\"I prefer…\", \"always…\", \"don't…\", a correction of your approach), facts about them, and stable facts about the machine (OS quirks, toolchains, paths, services).
- A task request is not a preference. \"Document X in the README\" or \"fix it in the library, not the tests\" describe one task; never record them as standing preferences or habits.
- Use target 'user' for the person, 'memory' for the machine and projects. Both are capped: batch your changes in one call and remove or merge stale entries to make room.
- Skip task progress, one-off details, anything easy to rediscover, and secrets.";

const SKILL_FOCUS: &str = "SKILLS — how to do a class of task for this user, faster next time.
A skill earns its place by making the next run take fewer steps or avoid a failure that actually happened here. Act when:
- the user corrected your approach, style or output;
- you hit an error and found the fix or workaround (wrong flags, missing tool, environment quirk);
- a skill that was loaded turned out wrong or incomplete in a way that cost you steps.
Preference order, take the first that fits:
1. patch a skill that was loaded this session;
2. patch an existing class-level skill that covers the territory (skill_manage list, then skill to read it);
3. add a support file (references/, templates/ or scripts/) under an existing skill and point to it from SKILL.md;
4. create a new class-level skill. Name it for the class of work, never for today's task, a PR, an error string or a single library.
Keep skills short: trigger, the few steps that matter with exact commands, pitfalls actually hit. Don't add ceremony (extra status checks, cleanups, re-verification) unless leaving it out caused a problem this session; every step you add is a tool call on every future run. Remove steps that proved unnecessary.
Never capture: environment failures the user can fix (missing binaries, unset credentials), negative claims about tools ('X doesn't work'), errors that a retry fixed, or one-off task narratives.";

const NOTHING: &str = "If nothing in the session meets the bar, reply 'Nothing to save.' and stop. A routine session that went smoothly usually has nothing to save.";

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
///
/// Skills are written from recurrence, not from one session: what this
/// session taught goes into the observation ledger first, and a new skill is
/// only written when the same problem class has come up in another session
/// too. A skill that was loaded and turned out wrong can be patched at once.
pub async fn review(cfg: &Config, paths: &Paths, session: &str, messages: &[Message], kind: Kind) -> Result<Outcome> {
    let key = crate::secrets::api_key(&cfg.api_key_secret);
    let client = llm::Client::new(&cfg.base_url, key, cfg.headers.clone())?;
    let memory = Store::new(paths, &cfg.memory);
    let (_tx, rx) = tokio::sync::watch::channel(false);
    let ctx = Arc::new(learner_ctx(cfg, paths, memory, rx));
    let wants_memory = matches!(kind, Kind::Memory | Kind::Both);
    let mut ledger_note = Vec::new();

    let loaded = skills_loaded(messages);
    // Observe whenever the gate fired: a procedure the user had to supply
    // reads as a preference to the gate, but it is exactly what recurs.
    let recurring = {
        let prior = ledger(paths);
        let new = observe(&client, cfg.learn_model(), messages, session).await.unwrap_or_default();
        append_ledger(paths, &new);
        for o in &new {
            ledger_note.push(format!("ledger: {}", o.problem_class));
        }
        recurrences(cfg, &new, &prior).await
    };
    let skills_ok = !loaded.is_empty() || !recurring.is_empty();
    if !wants_memory && !skills_ok {
        let summary = if ledger_note.is_empty() {
            "Nothing to save.".to_string()
        } else {
            format!("Recorded {} observation(s); a skill is written if they recur.", ledger_note.len())
        };
        return Ok(Outcome { summary, actions: ledger_note });
    }

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
    if wants_memory {
        let _ = writeln!(task, "{MEMORY_FOCUS}\n");
    }
    if skills_ok {
        let _ = writeln!(task, "{SKILL_FOCUS}\n");
        if recurring.is_empty() {
            let _ = writeln!(task, "Only patch the skills loaded this session ({}); don't create new skills. Anything else this session taught is already recorded and will become a skill if it recurs.\n", loaded.join(", "));
        } else {
            let _ = writeln!(task, "RECURRING PROBLEMS: these came up in more than one session, so they have earned a skill. Write or patch one class-level skill per problem class (fold it into an existing skill when one covers the territory). Use only what the evidence shows.");
            for (o, earlier) in &recurring {
                for e in std::iter::once(o).chain(earlier) {
                    let _ = writeln!(task, "- [session {}] {}: {} | fix: {} | evidence: {}", e.session, e.problem_class, e.summary, e.fix, llm::truncate(&e.evidence, 300));
                }
            }
            if !loaded.is_empty() {
                let _ = writeln!(task, "You may also patch the skills loaded this session ({}).", loaded.join(", "));
            }
            let _ = writeln!(task, "Don't create skills for anything else.\n");
        }
    }
    let _ = writeln!(task, "{NOTHING}");

    let mut groups = vec![];
    if wants_memory {
        groups.push(Group::Memory);
    }
    if skills_ok {
        groups.extend([Group::Skills, Group::SkillWrite]);
    }
    let defs = tools::definitions(&groups, &ctx);
    let mut out = run_loop(&client, cfg.learn_model(), &ctx, vec![Message::system(SYSTEM), Message::user(task)], &defs, 10).await?;
    ledger_note.append(&mut out.actions);
    out.actions = ledger_note;
    Ok(out)
}

/// The learner only gets memory and skill tools, but its context still
/// carries a sandbox: read-only, so nothing it does can write the workspace.
fn learner_ctx(cfg: &Config, paths: &Paths, memory: Store, rx: tokio::sync::watch::Receiver<bool>) -> Ctx {
    let cwd = std::env::current_dir().unwrap_or_default();
    let mut sb = cfg.sandbox.clone();
    if sb.mode != "off" {
        sb.mode = "read-only".into();
    }
    let policy = crate::sandbox::Policy::new(&sb, &cwd, paths);
    Ctx::new(cwd, paths.clone(), memory, rx, None, policy)
}

// ---------------------------------------------------------------- ledger

/// One thing a session taught, kept until it recurs. `problem_class` is what
/// recurrence is matched on: the kind of problem, not the task.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
#[serde(default)]
pub struct Observation {
    pub ts: u64,
    pub session: String,
    pub cwd: String,
    pub problem_class: String,
    pub summary: String,
    pub fix: String,
    pub evidence: String,
}

const OBSERVE: &str = "You read a finished agent session and note what it taught that would help a future, different task. Only these count:
- a workaround for the assistant's own failed approach (wrong flags, a missing tool and what replaced it, an environment or project quirk);
- a procedure or fact the user had to supply that the assistant couldn't have found;
- a pitfall that cost several steps.
Not: the task itself, failures in the user's code that the task was about, things a retry fixed, or anything already obvious from the repo.
Reply with JSON only: {\"observations\": [{\"problem_class\": \"...\", \"summary\": \"...\", \"fix\": \"...\", \"evidence\": \"...\"}]}
- problem_class: a few words naming the class of problem, general enough to recur (\"doctest inside markdown fences\", \"SSO token expiry before aws calls\"), never today's task or file names.
- summary: one sentence. fix: the exact commands or steps that worked. evidence: the error text that showed it.
Most sessions teach nothing: reply {\"observations\": []}.";

fn ledger_path(paths: &Paths) -> std::path::PathBuf {
    paths.home.join("learn").join("ledger.jsonl")
}

pub fn ledger(paths: &Paths) -> Vec<Observation> {
    std::fs::read_to_string(ledger_path(paths))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

fn append_ledger(paths: &Paths, obs: &[Observation]) {
    use std::io::Write;
    if obs.is_empty() {
        return;
    }
    let p = ledger_path(paths);
    let _ = std::fs::create_dir_all(p.parent().unwrap());
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(p) {
        for o in obs {
            let _ = writeln!(f, "{}", serde_json::to_string(o).unwrap_or_default());
        }
    }
}

async fn observe(client: &llm::Client, model: &str, messages: &[Message], session: &str) -> Result<Vec<Observation>> {
    let msgs = vec![Message::system(OBSERVE), Message::user(format!("<session>\n{}\n</session>", digest(messages, 40_000)))];
    let c = client.complete(llm::Request { model, messages: &msgs, tools: &[], max_tokens: Some(1500), temperature: None }).await?;
    let text = c.message.content.unwrap_or_default();
    let json_text = text.find('{').and_then(|a| text.rfind('}').map(|b| &text[a..=b])).unwrap_or("{}");
    let v: Value = serde_json::from_str(json_text).unwrap_or(json!({}));
    let cwd = std::env::current_dir().map(|p| p.display().to_string()).unwrap_or_default();
    let ts = crate::skills::now();
    Ok(v["observations"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|o| {
            let class = o["problem_class"].as_str()?.trim();
            (!class.is_empty()).then(|| Observation {
                ts,
                session: session.to_string(),
                cwd: cwd.clone(),
                problem_class: llm::truncate(class, 80),
                summary: llm::truncate(o["summary"].as_str().unwrap_or(""), 300),
                fix: llm::truncate(o["fix"].as_str().unwrap_or(""), 600),
                evidence: llm::truncate(o["evidence"].as_str().unwrap_or(""), 400),
            })
        })
        .take(5)
        .collect())
}

/// For each new observation, the earlier ones (from other sessions) that are
/// the same class of problem. Jev decides when a key resolves; word overlap
/// on the problem class otherwise.
async fn recurrences(cfg: &Config, new: &[Observation], prior: &[Observation]) -> Vec<(Observation, Vec<Observation>)> {
    let mut out = Vec::new();
    for o in new {
        let others: Vec<&Observation> = prior.iter().filter(|p| p.session != o.session).rev().take(60).collect();
        if others.is_empty() {
            continue;
        }
        let jev = match crate::secrets::get("TYPESAFE_API_KEY").filter(|_| cfg.router.mode != "local" && cfg.router.mode != "off") {
            Some(key) => same_problem_jev(cfg, &key, o, &others).await.ok(),
            None => None,
        };
        let matched: Vec<Observation> = match jev {
            Some(idx) => idx.into_iter().map(|i| others[i].clone()).collect(),
            None => others
                .iter()
                .filter(|p| {
                    p.problem_class.eq_ignore_ascii_case(&o.problem_class)
                        || crate::router::overlap(&o.problem_class, &p.problem_class) >= 0.5
                })
                .map(|p| (*p).clone())
                .collect(),
        };
        if !matched.is_empty() {
            out.push((o.clone(), matched));
        }
    }
    out
}

async fn same_problem_jev(cfg: &Config, key: &str, o: &Observation, others: &[&Observation]) -> Result<Vec<usize>, String> {
    let mut criteria = serde_json::Map::new();
    // "none" first: Jev favours the first option (see src/router.rs).
    criteria.insert("none".into(), json!("none of these is the same kind of problem"));
    for (i, p) in others.iter().enumerate() {
        criteria.insert(format!("o{i}"), json!(format!("{}: {}", p.problem_class, llm::truncate(&p.summary, 200))));
    }
    let mut q = serde_json::Map::new();
    q.insert(
        "same".into(),
        json!({ "type": "choice", "instructions": "Which earlier observation is the same kind of problem as `new` (would be solved by the same procedure), if any?", "criteria": criteria }),
    );
    let state = json!({ "new": format!("{}: {}", o.problem_class, o.summary) });
    let v = crate::jev::ask(&cfg.router, &reqwest::Client::new(), key, "recurrence", state, q, std::time::Duration::from_secs(5)).await?;
    let probs = v["answers"]["same"]["probabilities"].as_object().cloned().unwrap_or_default();
    Ok(probs
        .iter()
        .filter(|(k, p)| *k != "none" && p.as_f64().unwrap_or(0.0) >= 0.4)
        .filter_map(|(k, _)| k[1..].parse().ok())
        .collect())
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
- tighten descriptions so it is obvious when each skill applies. A skill suggested far more often than it is viewed has a description that attracts the wrong requests: rewrite it to say plainly what it is for and what it isn't.
Never touch pinned skills. Deleting only archives, but be conservative: when unsure, leave a skill alone. Reply with one line summarizing what you changed.";

pub async fn curate(cfg: &Config, paths: &Paths) -> Result<Option<Outcome>> {
    let key = crate::secrets::api_key(&cfg.api_key_secret);
    let client = llm::Client::new(&cfg.base_url, key, cfg.headers.clone())?;
    let memory = Store::new(paths, &cfg.memory);
    let (_tx, rx) = tokio::sync::watch::channel(false);
    let ctx = learner_ctx(cfg, paths, memory, rx);
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
        let _ = writeln!(listing, "- {} (suggested {}, views {}, patches {}, last used {idle}): {}", s.name, u.suggested, u.views, u.patches, s.description);
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

/// The verdict of the cheap check that runs before an LLM review.
pub struct Gate {
    pub kind: Option<Kind>,
    pub source: &'static str,
    pub note: String,
}

fn user_texts(messages: &[Message]) -> Vec<String> {
    messages
        .iter()
        .filter(|m| m.role == "user")
        .filter_map(|m| m.content.as_deref())
        .map(|c| c.split("<system-reminder>").next().unwrap_or("").trim().to_string())
        .filter(|c| !c.is_empty())
        .collect()
}

/// Tool results that report a failure, with the call that produced them.
fn tool_errors(messages: &[Message]) -> Vec<String> {
    let mut calls = std::collections::HashMap::new();
    for m in messages {
        for c in &m.tool_calls {
            calls.insert(c.id.clone(), format!("{} {}", c.function.name, llm::truncate(&c.function.arguments, 160)));
        }
    }
    messages
        .iter()
        .filter(|m| m.role == "tool")
        .filter_map(|m| {
            let out = m.content.as_deref()?;
            out.starts_with("Error:").then(|| {
                let call = m.tool_call_id.as_ref().and_then(|id| calls.get(id)).cloned().unwrap_or_default();
                format!("{call} -> {}", llm::truncate(out, 240))
            })
        })
        .collect()
}

fn skills_loaded(messages: &[Message]) -> Vec<String> {
    messages
        .iter()
        .flat_map(|m| &m.tool_calls)
        .filter(|c| c.function.name == "skill")
        .filter_map(|c| serde_json::from_str::<Value>(&c.function.arguments).ok()?["name"].as_str().map(String::from))
        .collect()
}

const PREFERENCE_HINTS: &[&str] = &[
    "i prefer", "i like", "i don't like", "i hate", "i want you to", "always ", "never ", "stop ", "don't ", "do not ",
    "remember", "from now on", "call me", "my name", "too verbose", "too long", "next time", "on my machine",
];

/// Decide whether a stretch of session holds anything worth an LLM review,
/// before paying for one. Jev answers when a TypeSafe key resolves; local
/// heuristics otherwise.
pub async fn gate(cfg: &Config, messages: &[Message]) -> Gate {
    let users = user_texts(messages);
    if users.is_empty() {
        return Gate { kind: None, source: "local", note: "no user turns".into() };
    }
    let errors = tool_errors(messages);
    let loaded = skills_loaded(messages);
    gate_parts(cfg, &users, &errors, &loaded).await
}

/// The gate on already-extracted evidence; `construct gate` feeds it directly.
pub async fn gate_parts(cfg: &Config, users: &[String], errors: &[String], loaded: &[String]) -> Gate {
    if users.is_empty() {
        return Gate { kind: None, source: "local", note: "no user turns".into() };
    }
    if cfg.router.mode != "local" && cfg.router.mode != "off" {
        if let Some(key) = crate::secrets::get("TYPESAFE_API_KEY") {
            match jev_gate(cfg, &key, users, errors, loaded).await {
                Ok(g) => return g,
                Err(e) => log_line_quiet(&format!("jev gate failed ({e}); using local heuristics")),
            }
        }
    }
    let joined = users.join("\n").to_lowercase();
    let pref = PREFERENCE_HINTS.iter().any(|h| joined.contains(h));
    let skill = !loaded.is_empty() || errors.len() >= 3;
    let kind = match (pref, skill) {
        (true, true) => Some(Kind::Both),
        (true, false) => Some(Kind::Memory),
        (false, true) => Some(Kind::Skills),
        _ => None,
    };
    Gate { kind, source: "local", note: format!("preference={pref} tool_errors={} skills_loaded={}", errors.len(), loaded.len()) }
}

async fn jev_gate(cfg: &Config, key: &str, users: &[String], errors: &[String], loaded: &[String]) -> Result<Gate, String> {
    let users: Vec<String> = users.iter().rev().take(12).rev().map(|u| llm::truncate(u, 500)).collect();
    let errors: Vec<String> = errors.iter().take(8).cloned().collect();
    let noul = |q: &str| json!({ "type": "noul", "instructions": q });
    // Each question sees only the state it needs: whether the user stated a
    // preference doesn't depend on tool errors, and long error text pulled
    // that answer around. So up to two calls, in parallel.
    let mut pref_q = serde_json::Map::new();
    // A choice, not a noul: asked as yes/no, Jev counted task phrasing ("I want
    // the parser to...", "make sure it always...") as preferences. "task" goes
    // first: Jev favours the first option (see src/router.rs).
    pref_q.insert(
        "preference".into(),
        json!({ "type": "choice", "instructions": "Across `user_messages`, what is the most lasting thing the user said?", "criteria": {
            "task": "only requests for this task, or corrections that apply to this task alone",
            "preference": "a rule for how the assistant should work in future sessions, or a durable fact about the user or their machine",
        } }),
    );
    let mut skill_q = serde_json::Map::new();
    if !errors.is_empty() {
        skill_q.insert(
            "workaround".into(),
            noul("Do `tool_errors` show the assistant's own commands or approach failing in a way that needed a different technique (wrong flags, missing tools, environment quirks), as opposed to failures in the user's code that the task was about?"),
        );
    }
    if !loaded.is_empty() {
        skill_q.insert(
            "skill_gap".into(),
            noul("Do `user_messages` or `tool_errors` suggest that a procedure listed in `skills_loaded` was wrong, incomplete or outdated?"),
        );
    }
    let http = reqwest::Client::new();
    let timeout = std::time::Duration::from_secs(5);
    let pref_call = crate::jev::ask(&cfg.router, &http, key, "gate", json!({ "user_messages": users }), pref_q, timeout);
    let skill_call = async {
        if skill_q.is_empty() {
            return Ok(json!({}));
        }
        let state = json!({ "user_messages": users, "tool_errors": errors, "skills_loaded": loaded });
        crate::jev::ask(&cfg.router, &http, key, "gate", state, skill_q, timeout).await
    };
    let (pv, sv) = tokio::join!(pref_call, skill_call);
    let (pv, sv) = (pv?, sv?);
    let v = json!({ "answers": {
        "preference": pv["answers"]["preference"],
        "workaround": sv["answers"]["workaround"],
        "skill_gap": sv["answers"]["skill_gap"],
    } });
    let p = |k: &str| v["answers"][k]["noul"].as_f64().unwrap_or(0.0);
    let pref_p = v["answers"]["preference"]["probabilities"]["preference"].as_f64().unwrap_or(0.0);
    let pref = pref_p >= 0.5;
    let skill = p("workaround") >= 0.5 || p("skill_gap") >= 0.5;
    let kind = match (pref, skill) {
        (true, true) => Some(Kind::Both),
        (true, false) => Some(Kind::Memory),
        (false, true) => Some(Kind::Skills),
        _ => None,
    };
    Ok(Gate {
        kind,
        source: "jev",
        note: format!("preference={pref_p:.2} workaround={:.2} skill_gap={:.2}", p("workaround"), p("skill_gap")),
    })
}

fn log_line_quiet(line: &str) {
    if let Ok(p) = Paths::resolve() {
        log_line(&p, line);
    }
}
