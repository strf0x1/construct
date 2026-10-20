//! The tool surface. Tools are grouped; the core group is always present and
//! the others are switched on per turn by the router, so the model never has
//! to pick from a huge list. Once on, a group stays on for the session: the
//! tool list then only ever grows, which keeps provider prefix caches warm.

pub mod edit;
pub mod fs;
pub mod shell;
pub mod web;

use crate::config::Paths;
use crate::mcp;
use crate::memory::{self, Target};
use crate::skills::Library;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::sync::watch;

pub const OUTPUT_MAX: usize = 30_000;

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Group {
    Core,
    Web,
    Memory,
    Skills,
    SkillWrite,
    History,
    Mcp(String),
}

impl Group {
    pub fn label(&self) -> String {
        match self {
            Group::Core => "core".into(),
            Group::Web => "web".into(),
            Group::Memory => "memory".into(),
            Group::Skills => "skills".into(),
            Group::SkillWrite => "skill-write".into(),
            Group::History => "history".into(),
            Group::Mcp(s) => format!("mcp:{s}"),
        }
    }
}

pub struct Ctx {
    pub cwd: PathBuf,
    pub paths: Paths,
    pub http: reqwest::Client,
    pub cancel: watch::Receiver<bool>,
    pub memory: memory::Store,
    pub skills: Library,
    pub mcp: Option<Arc<mcp::Manager>>,
    pub sandbox: crate::sandbox::Policy,
    read_files: Mutex<HashSet<PathBuf>>,
}

impl Ctx {
    pub fn new(
        cwd: PathBuf,
        paths: Paths,
        memory: memory::Store,
        cancel: watch::Receiver<bool>,
        mcp: Option<Arc<mcp::Manager>>,
        sandbox: crate::sandbox::Policy,
    ) -> Self {
        let http = reqwest::Client::builder()
            .user_agent(concat!("construct/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(std::time::Duration::from_secs(15))
            .build()
            .expect("http client");
        let skills = Library::new(&paths);
        Self { cwd, paths, http, cancel, memory, skills, mcp, sandbox, read_files: Mutex::new(HashSet::new()) }
    }
    pub fn mark_read(&self, p: &Path) {
        self.read_files.lock().unwrap().insert(p.to_path_buf());
    }
    pub fn was_read(&self, p: &Path) -> bool {
        self.read_files.lock().unwrap().contains(p)
    }
}

fn def(name: &str, description: &str, params: Value) -> Value {
    json!({ "type": "function", "function": { "name": name, "description": description, "parameters": params } })
}

fn obj(props: Value, required: &[&str]) -> Value {
    json!({ "type": "object", "properties": props, "required": required })
}

/// Tool definitions for the given groups, in a stable order.
pub fn definitions(groups: &[Group], ctx: &Ctx) -> Vec<Value> {
    let mut out = Vec::new();
    let mut groups = groups.to_vec();
    groups.sort();
    groups.dedup();
    for g in &groups {
        match g {
            Group::Core => out.extend(core()),
            Group::Web => out.extend(web_defs()),
            Group::Memory => out.push(memory_def()),
            Group::Skills => out.push(skill_def(ctx)),
            Group::SkillWrite => out.push(skill_manage_def()),
            Group::History => out.push(history_def()),
            Group::Mcp(server) => {
                if let Some(m) = &ctx.mcp {
                    out.extend(m.definitions(server));
                }
            }
        }
    }
    out
}

fn core() -> Vec<Value> {
    vec![
        def(
            "shell",
            "Run a bash command and return its combined stdout/stderr. Use for builds, tests, git, and anything without a dedicated tool. Default timeout 120s.",
            obj(
                json!({
                    "command": { "type": "string" },
                    "workdir": { "type": "string", "description": "Directory to run in; defaults to the session's" },
                    "timeout": { "type": "integer", "description": "Seconds, max 1800" }
                }),
                &["command"],
            ),
        ),
        def(
            "read",
            "Read a text file with line numbers (or list a directory). Reads up to 2000 lines; use offset/limit for more.",
            obj(
                json!({
                    "path": { "type": "string" },
                    "offset": { "type": "integer", "description": "1-based first line" },
                    "limit": { "type": "integer" }
                }),
                &["path"],
            ),
        ),
        def(
            "write",
            "Create a file or replace its whole content. An existing file must be read first. Prefer edit for changes.",
            obj(json!({ "path": { "type": "string" }, "content": { "type": "string" } }), &["path", "content"]),
        ),
        def(
            "edit",
            "Replace exact text in a file. old_string must match once (copy it from read output without the line-number prefix) unless replace_all is set. Tolerates quote/dash and trailing-whitespace differences.",
            obj(
                json!({
                    "path": { "type": "string" },
                    "old_string": { "type": "string" },
                    "new_string": { "type": "string" },
                    "replace_all": { "type": "boolean" }
                }),
                &["path", "old_string", "new_string"],
            ),
        ),
        def(
            "grep",
            "Search file contents with a regex (ripgrep engine, respects .gitignore). Returns path:line: text.",
            obj(
                json!({
                    "pattern": { "type": "string" },
                    "path": { "type": "string", "description": "File or directory; defaults to the session's" },
                    "include": { "type": "string", "description": "Glob filter, e.g. *.rs" },
                    "ignore_case": { "type": "boolean" }
                }),
                &["pattern"],
            ),
        ),
        def(
            "glob",
            "Find files by glob pattern (e.g. src/**/*.rs), newest first. Respects .gitignore.",
            obj(json!({ "pattern": { "type": "string" }, "path": { "type": "string" } }), &["pattern"]),
        ),
    ]
}

fn web_defs() -> Vec<Value> {
    vec![
        def(
            "web_search",
            "Search the web. Returns titles, URLs and snippets.",
            obj(json!({ "query": { "type": "string" }, "limit": { "type": "integer" } }), &["query"]),
        ),
        def(
            "web_fetch",
            "Fetch a URL and return it as markdown (HTML is converted).",
            obj(json!({ "url": { "type": "string" } }), &["url"]),
        ),
    ]
}

fn memory_def() -> Value {
    def(
        "memory",
        "Durable memory injected into every future session. target 'user' = who the user is and how they like to work; 'memory' = facts about this machine, projects and tools. Both are capped: pass all changes as one operations array (applied atomically, cap checked on the result), removing stale entries to make room. Save preferences, corrections and stable facts; skip task progress and things easy to rediscover.",
        obj(
            json!({
                "target": { "type": "string", "enum": ["user", "memory"] },
                "operations": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "action": { "type": "string", "enum": ["add", "replace", "remove"] },
                            "content": { "type": "string" },
                            "old_text": { "type": "string", "description": "Unique substring of the entry to replace/remove" }
                        },
                        "required": ["action"]
                    }
                }
            }),
            &["target", "operations"],
        ),
    )
}

fn skill_def(_ctx: &Ctx) -> Value {
    def(
        "skill",
        "Load a skill: a procedure written down from past work. Returns its SKILL.md and linked files; pass file to read one of those.",
        obj(json!({ "name": { "type": "string" }, "file": { "type": "string" } }), &["name"]),
    )
}

fn skill_manage_def() -> Value {
    def(
        "skill_manage",
        "Write the skill library. create (content = full SKILL.md with frontmatter name/description), patch (old_string/new_string, optional file), edit (full rewrite), write_file (file under references/, templates/ or scripts/), remove_file, delete (archives; absorbed_into names the skill that took over), list. Skills are class-level procedures: trigger, numbered steps with exact commands, pitfalls, how to verify. The description is how the skill gets found later: models deciding whether a skill fits their task see only its name and description, so say plainly what situations it is for (and what it isn't, if a similar skill exists) and keep the steps in the body.",
        obj(
            json!({
                "action": { "type": "string", "enum": ["create", "patch", "edit", "write_file", "remove_file", "delete", "list"] },
                "name": { "type": "string" },
                "content": { "type": "string" },
                "file": { "type": "string" },
                "old_string": { "type": "string" },
                "new_string": { "type": "string" },
                "absorbed_into": { "type": "string" }
            }),
            &["action"],
        ),
    )
}

fn history_def() -> Value {
    def(
        "session_search",
        "Search the user's past agent sessions (all harnesses, via chatstrata) by keywords. Use when the user refers to earlier work or you need a past decision.",
        obj(
            json!({
                "query": { "type": "string" },
                "limit": { "type": "integer" },
                "since": { "type": "string", "description": "YYYY-MM-DD" }
            }),
            &["query"],
        ),
    )
}

/// Tools that change nothing can run side by side.
pub fn is_readonly(name: &str) -> bool {
    matches!(name, "read" | "grep" | "glob" | "web_search" | "web_fetch" | "skill" | "session_search")
}

pub async fn call(ctx: &Ctx, name: &str, args: &str) -> Result<String, String> {
    let a: Value = if args.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(args).map_err(|e| format!("arguments are not valid JSON ({e}); resend the call"))?
    };
    match name {
        "shell" => shell::run(ctx, &a).await,
        "read" => blocking(ctx, a, fs::read).await,
        "write" => fs::write(ctx, &a),
        "edit" => fs::edit(ctx, &a),
        "grep" => blocking(ctx, a, fs::grep).await,
        "glob" => blocking(ctx, a, fs::glob).await,
        "web_fetch" => web::fetch(ctx, &a).await,
        "web_search" => web::search(ctx, &a).await,
        "memory" => memory_call(ctx, &a),
        "skill" => {
            let n = a["name"].as_str().ok_or("name is required")?;
            ctx.skills.view(n, a["file"].as_str())
        }
        "skill_manage" => skill_manage(ctx, &a),
        "session_search" => session_search(&a).await,
        other if other.starts_with("mcp__") => match &ctx.mcp {
            Some(m) => m.call(other, a).await,
            None => Err(format!("unknown tool {other}")),
        },
        other => Err(format!("unknown tool {other}")),
    }
}

/// Filesystem walks are synchronous; keep them off the async workers.
async fn blocking(ctx: &Ctx, a: Value, f: fn(&Ctx, &Value) -> Result<String, String>) -> Result<String, String> {
    // The walk borrows ctx, so use block_in_place rather than spawn_blocking.
    let r = tokio::task::block_in_place(|| f(ctx, &a))?;
    Ok(cap_output(ctx, "out", &r))
}

fn memory_call(ctx: &Ctx, a: &Value) -> Result<String, String> {
    let target: Target = serde_json::from_value(a["target"].clone()).map_err(|_| "target must be user or memory")?;
    let ops: Vec<memory::Op> = match a.get("operations") {
        Some(v) => serde_json::from_value(v.clone()).map_err(|e| format!("operations: {e}"))?,
        None => vec![serde_json::from_value(a.clone()).map_err(|e| format!("{e}"))?],
    };
    ctx.memory.apply(target, &ops)
}

fn skill_manage(ctx: &Ctx, a: &Value) -> Result<String, String> {
    let s = |k: &str| a.get(k).and_then(Value::as_str);
    let name = || s("name").ok_or_else(|| "name is required".to_string());
    match s("action").unwrap_or("") {
        "list" => {
            let list = ctx.skills.list();
            if list.is_empty() {
                return Ok("the skill library is empty".into());
            }
            Ok(list.iter().map(|k| format!("- {}: {}", k.name, k.description)).collect::<Vec<_>>().join("\n"))
        }
        "create" => ctx.skills.create(&name()?, s("content").ok_or("content is required")?, "agent"),
        "edit" => ctx.skills.edit(&name()?, s("content").ok_or("content is required")?),
        "patch" => ctx.skills.patch(
            &name()?,
            s("file"),
            s("old_string").ok_or("old_string is required")?,
            s("new_string").ok_or("new_string is required")?,
        ),
        "write_file" => ctx.skills.write_file(&name()?, s("file").ok_or("file is required")?, s("content").unwrap_or("")),
        "remove_file" => ctx.skills.remove_file(&name()?, s("file").ok_or("file is required")?),
        "delete" => ctx.skills.archive(&name()?, s("absorbed_into")),
        other => Err(format!("unknown action {other:?}")),
    }
}

async fn session_search(a: &Value) -> Result<String, String> {
    let q = a["query"].as_str().ok_or("query is required")?;
    let limit = a["limit"].as_u64().unwrap_or(8).clamp(1, 30).to_string();
    let mut cmd = tokio::process::Command::new("chatstrata");
    cmd.args(["search", "--json", "--limit", &limit]);
    if let Some(since) = a["since"].as_str() {
        cmd.args(["--since", since]);
    }
    cmd.arg(q);
    let out = cmd
        .output()
        .await
        .map_err(|e| format!("chatstrata isn't available ({e}); install it to search past sessions"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).to_string());
    }
    let hits: Vec<Value> = serde_json::from_slice(&out.stdout).unwrap_or_default();
    if hits.is_empty() {
        return Ok("no past sessions matched".into());
    }
    Ok(hits
        .iter()
        .map(|h| {
            format!(
                "[{} · {} · {}] {}\n  {}",
                h["created_at"].as_str().unwrap_or("").get(..16).unwrap_or(""),
                h["source"].as_str().unwrap_or(""),
                h["project"].as_str().unwrap_or(""),
                crate::llm::truncate(h["title"].as_str().unwrap_or(""), 80),
                h["snippet"].as_str().unwrap_or("").replace('\n', " ")
            )
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

/// Keep tool output inside the context budget. Long output keeps its head and
/// tail; the whole thing is spilled to a file the model can read or grep.
pub fn cap_output(ctx: &Ctx, kind: &str, text: &str) -> String {
    if text.len() <= OUTPUT_MAX {
        return text.to_string();
    }
    let dir = ctx.paths.spill_dir();
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join(format!("{kind}-{}-{}.txt", std::process::id(), crate::skills::now_nanos()));
    let saved = std::fs::write(&path, text).is_ok();
    let head = crate::llm::truncate(text, OUTPUT_MAX / 2);
    let mut tail_start = text.len() - OUTPUT_MAX / 3;
    while !text.is_char_boundary(tail_start) {
        tail_start += 1;
    }
    let lines = text.lines().count();
    format!(
        "{head}\n\n[… {} bytes, {lines} lines total{} …]\n\n{}",
        text.len(),
        if saved { format!("; full output in {}", path.display()) } else { String::new() },
        &text[tail_start..]
    )
}
