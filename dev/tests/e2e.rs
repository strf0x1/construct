//! End-to-end tests: the real `construct` binary, a fresh home per test, and
//! a scripted model endpoint. Each test checks what a user (or the model)
//! would observe: files on disk, what the harness sent, what the screen shows.

use construct_dev::{drive, serve};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Once;
use std::time::Duration;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

fn construct() -> PathBuf {
    static BUILD: Once = Once::new();
    BUILD.call_once(|| {
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let ok = Command::new(cargo).args(["build", "-q", "-p", "construct"]).current_dir(root()).status().unwrap();
        assert!(ok.success(), "building construct failed");
    });
    root().join("target/debug/construct")
}

fn dev_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_construct-dev"))
}

struct Env {
    dir: PathBuf,
    home: PathBuf,
    work: PathBuf,
    port: u16,
    _rt: tokio::runtime::Runtime,
}

impl Env {
    fn new(name: &str, steps: Value) -> Self {
        let dir = std::env::temp_dir().join(format!("construct-e2e-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let home = dir.join("home");
        let work = dir.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        let cfg = serve::Config {
            script: serde_json::from_value(steps["steps"].clone()).unwrap(),
            repeat_call: None,
            upstream: None,
            key: None,
            log: Some(dir.join("log.jsonl")),
            dump_dir: Some(dir.join("dump")),
            chunk_ms: 1,
        };
        let (port, _) = rt.block_on(serve::serve(cfg, 0)).unwrap();
        Self { dir, home, work, port, _rt: rt }
    }

    fn env(&self) -> Vec<(String, String)> {
        vec![
            ("CONSTRUCT_HOME".into(), self.home.display().to_string()),
            ("CONSTRUCT_BASE_URL".into(), format!("http://127.0.0.1:{}/v1", self.port)),
            ("OPENAI_API_KEY".into(), "test".into()),
            ("TYPESAFE_API_KEY".into(), String::new()),
            ("CONSTRUCT_LEARN_DEBOUNCE_MS".into(), "0".into()),
        ]
    }

    fn config(&self, toml: &str) {
        std::fs::create_dir_all(&self.home).unwrap();
        std::fs::write(self.home.join("config.toml"), toml).unwrap();
    }

    fn run(&self, args: &[&str]) -> Output {
        let mut c = Command::new(construct());
        c.args(args).current_dir(&self.work).env_remove("TYPESAFE_API_KEY");
        for (k, v) in self.env() {
            if !v.is_empty() {
                c.env(k, v);
            }
        }
        c.output().unwrap()
    }

    /// Every request body the harness sent, in order.
    fn requests(&self) -> Vec<Value> {
        let mut files: Vec<PathBuf> = std::fs::read_dir(self.dir.join("dump")).map(|r| r.flatten().map(|e| e.path()).collect()).unwrap_or_default();
        files.sort();
        files.iter().map(|p| serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap()).collect()
    }
}

fn tool_names(req: &Value) -> Vec<String> {
    req["tools"].as_array().map(|t| t.iter().map(|x| x["function"]["name"].as_str().unwrap().to_string()).collect()).unwrap_or_default()
}

fn last_tool_result(req: &Value) -> String {
    req["messages"].as_array().unwrap().iter().rev().find(|m| m["role"] == "tool").map(|m| m["content"].as_str().unwrap_or("").to_string()).unwrap_or_default()
}

fn text(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

#[test]
fn headless_turn_reads_edits_and_runs_tools_in_parallel() {
    let env = Env::new(
        "edit",
        json!({ "steps": [
            { "call": "read", "args": { "path": "notes.txt" } },
            { "call": "edit", "args": { "path": "notes.txt", "old_string": "colour = \u{201C}red\u{201D}", "new_string": "colour = \"blue\"" } },
            { "calls": [ { "call": "shell", "args": { "command": "cat notes.txt" } }, { "call": "grep", "args": { "pattern": "blue" } } ] },
            { "text": "Changed the colour to blue." }
        ] }),
    );
    std::fs::write(env.work.join("notes.txt"), "title: test\ncolour = \u{201C}red\u{201D}\n").unwrap();
    let out = env.run(&["run", "--no-learn", "make it blue"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(text(&out).contains("Changed the colour to blue."));
    // The edit landed even though the model typed straight quotes.
    assert_eq!(std::fs::read_to_string(env.work.join("notes.txt")).unwrap(), "title: test\ncolour = \"blue\"\n");
    let reqs = env.requests();
    assert_eq!(reqs.len(), 4);
    assert!(last_tool_result(&reqs[1]).contains("colour"), "read result reached the model");
    // Both parallel results came back, paired with their calls.
    let msgs = reqs[3]["messages"].as_array().unwrap();
    let tools: Vec<&Value> = msgs.iter().filter(|m| m["role"] == "tool").collect();
    assert_eq!(tools.len(), 4);
    assert!(tools[2]["content"].as_str().unwrap().contains("blue"));
    assert!(tools[3]["content"].as_str().unwrap().contains("notes.txt:2"));
    // The session log is complete JSONL.
    let sessions: Vec<_> = std::fs::read_dir(env.home.join("sessions")).unwrap().flatten().collect();
    assert_eq!(sessions.len(), 1);
    let log = std::fs::read_to_string(sessions[0].path()).unwrap();
    assert!(log.lines().all(|l| serde_json::from_str::<Value>(l).is_ok()));
    assert!(log.contains("\"harness\":\"construct\""));
}

#[test]
fn first_request_is_lean_and_router_adds_groups_on_demand() {
    let env = Env::new("router", json!({ "steps": [ { "text": "ok" }, { "text": "ok" }, { "text": "ok" } ] }));
    env.config("[router]\nmode = \"local\"\n");
    // A skill the router should surface for a matching request.
    let skill = env.home.join("skills/release-crate/SKILL.md");
    std::fs::create_dir_all(skill.parent().unwrap()).unwrap();
    std::fs::write(&skill, "---\nname: release-crate\ndescription: Publish a release of a Rust crate: bump version, changelog, tag\n---\n\n1. bump\n").unwrap();

    let out = env.run(&["run", "--no-learn", "explain what this function does"]);
    assert!(out.status.success(), "{}", text(&out));
    let first = &env.requests()[0];
    let names = tool_names(first);
    assert_eq!(names, ["shell", "read", "write", "edit", "grep", "glob", "enable_tools"], "plain turn gets only core tools");
    let bytes = serde_json::to_vec(first).unwrap().len();
    assert!(bytes < 6000, "first request is {bytes} bytes");

    let out = env.run(&["run", "--no-learn", "publish a new release of this crate with a changelog"]);
    assert!(out.status.success());
    let req = &env.requests()[1];
    assert!(tool_names(req).contains(&"skill".to_string()));
    let user = req["messages"].as_array().unwrap().last().unwrap()["content"].as_str().unwrap().to_string();
    assert!(user.contains("release-crate"), "skill suggested on the user turn: {user}");
    assert!(!req["messages"][0]["content"].as_str().unwrap().contains("release-crate"), "no skill index in the system prompt");

    let out = env.run(&["run", "--no-learn", "summarize https://example.com/changelog"]);
    assert!(out.status.success());
    let names = tool_names(&env.requests()[2]);
    assert!(names.contains(&"web_fetch".to_string()) && names.contains(&"web_search".to_string()));
}

#[test]
fn model_can_enable_a_group_the_router_missed() {
    let env = Env::new(
        "enable",
        json!({ "steps": [
            { "call": "enable_tools", "args": { "groups": ["memory"] } },
            { "call": "memory", "args": { "target": "user", "operations": [ { "action": "add", "content": "Prefers terse answers" } ] } },
            { "text": "noted" }
        ] }),
    );
    env.config("[router]\nmode = \"local\"\n");
    let out = env.run(&["run", "--no-learn", "keep it short from here"]);
    assert!(out.status.success(), "{}", text(&out));
    let reqs = env.requests();
    assert!(!tool_names(&reqs[0]).contains(&"memory".to_string()));
    assert!(tool_names(&reqs[1]).contains(&"memory".to_string()), "group switched on for the next step");
    assert!(std::fs::read_to_string(env.home.join("memory/USER.md")).unwrap().contains("Prefers terse answers"));
}

#[test]
fn learning_pass_writes_capped_memory_and_a_skill() {
    let skill_md = "---\nname: rust-crate-release\ndescription: Release a Rust crate with changelog and tag\n---\n\n1. cargo test\n2. bump version\n3. git tag\n";
    let big = "x".repeat(1500);
    let env = Env::new(
        "learn",
        json!({ "steps": [
            { "call": "shell", "args": { "command": "ls /nonexistent-release-dir" } },
            { "call": "shell", "args": { "command": "exit 2" } },
            { "call": "shell", "args": { "command": "false" } },
            { "text": "released" },
            // learning pass:
            { "call": "memory", "args": { "target": "user", "operations": [ { "action": "add", "content": big } ] } },
            { "call": "memory", "args": { "target": "user", "operations": [ { "action": "add", "content": "Releases crates on Fridays" } ] } },
            { "call": "skill_manage", "args": { "action": "create", "name": "rust-crate-release", "content": skill_md } },
            { "text": "Saved a preference and a release skill." }
        ] }),
    );
    env.config("[memory]\nuser_chars = 1000\n");
    let out = env.run(&["run", "--no-learn", "release the crate. I always do this on Fridays."]);
    assert!(out.status.success(), "{}", text(&out));
    let id = std::fs::read_dir(env.home.join("sessions")).unwrap().flatten().next().unwrap().file_name();
    let id = id.to_string_lossy().trim_end_matches(".jsonl").to_string();

    let out = env.run(&["learn", "--session", &id]);
    assert!(out.status.success(), "{}", text(&out));
    let reqs = env.requests();
    let review = &reqs[4];
    let names = tool_names(review);
    assert!(names.contains(&"memory".into()) && names.contains(&"skill_manage".into()));
    assert!(!names.contains(&"shell".into()), "the learning pass can only touch memory and skills");
    assert!(review["messages"][1]["content"].as_str().unwrap().contains("USER: release the crate"));
    // The over-cap write was refused and the model was told why.
    assert!(last_tool_result(&reqs[5]).contains("over the cap"));
    let user = std::fs::read_to_string(env.home.join("memory/USER.md")).unwrap();
    assert_eq!(user, "Releases crates on Fridays");
    let skill = std::fs::read_to_string(env.home.join("skills/rust-crate-release/SKILL.md")).unwrap();
    assert!(skill.contains("3. git tag"));
    let usage: Value = serde_json::from_str(&std::fs::read_to_string(env.home.join("skills/.usage.json")).unwrap()).unwrap();
    assert_eq!(usage["rust-crate-release"]["author"], "agent");
    // The next session starts knowing the user.
    let out = env.run(&["run", "--no-learn", "hi"]);
    assert!(out.status.success());
    let sys = env.requests().last().unwrap()["messages"][0]["content"].as_str().unwrap().to_string();
    assert!(sys.contains("Releases crates on Fridays"));
}

#[test]
fn mcp_server_starts_lazily_when_routed() {
    let env = Env::new(
        "mcp",
        json!({ "steps": [
            { "text": "hello" },
            { "call": "mcp__calc__add", "args": { "a": 2, "b": 3 } },
            { "text": "2 + 3 = 5" }
        ] }),
    );
    env.config(&format!(
        "[router]\nmode = \"local\"\n[mcp.calc]\ncommand = \"{}\"\nargs = [\"mcp-fake\"]\ndescription = \"calculator arithmetic add numbers\"\n",
        dev_bin().display()
    ));
    let out = env.run(&["run", "--no-learn", "say hello"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(!tool_names(&env.requests()[0]).iter().any(|n| n.starts_with("mcp__")), "unrelated turn: no MCP tools");
    let out = env.run(&["run", "--no-learn", "use the calc server to add 2 and 3"]);
    assert!(out.status.success(), "{}", text(&out));
    let reqs = env.requests();
    assert!(tool_names(&reqs[1]).contains(&"mcp__calc__add".to_string()));
    assert_eq!(last_tool_result(&reqs[2]), "5");
}

#[test]
fn tui_streams_a_turn_and_esc_interrupts_a_slow_one() {
    let env = Env::new(
        "tui",
        json!({ "steps": [
            { "call": "shell", "args": { "command": "echo from-the-shell" } },
            { "text": "## Done\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\nAll **good**." },
            { "text": "never shown", "delay_ms": 20000 },
            { "text": "back again" }
        ] }),
    );
    let mut d = drive::Driver::spawn(&[construct().display().to_string()], &env.env(), Some(env.work.to_str().unwrap()), 100, 30).unwrap();
    let mut log = Vec::new();
    let script = r#"
wait "ask construct" 3000
type run it
key enter
wait "from-the-shell" 5000
wait "All good." 5000
expect "│ 2"
type slow one
key enter
wait "thinking" 3000
key esc
wait "interrupted" 3000
type again
key enter
wait "back again" 5000
"#;
    let r = drive::run_script(&mut d, script, &mut log);
    if let Err(e) = &r {
        panic!("{e:#}\n{}", String::from_utf8_lossy(&log));
    }
    let m = r.unwrap();
    let first = m["first_frame_ms"].as_f64().unwrap();
    assert!(first < 500.0, "first frame took {first}ms");
    d.send(b"/quit\r").unwrap();
    std::thread::sleep(Duration::from_millis(300));
    assert!(d.exited() || { d.pump(Duration::from_millis(500)); d.exited() }, "quit exits");
}

#[test]
fn a_session_with_nothing_to_learn_costs_no_review() {
    let env = Env::new("gate", json!({ "steps": [ { "call": "read", "args": { "path": "a.txt" } }, { "text": "It says hello." } ] }));
    env.config("[router]\nmode = \"local\"\n");
    std::fs::write(env.work.join("a.txt"), "hello\n").unwrap();
    let out = env.run(&["run", "--no-learn", "what does a.txt say?"]);
    assert!(out.status.success(), "{}", text(&out));
    let id = std::fs::read_dir(env.home.join("sessions")).unwrap().flatten().next().unwrap().file_name();
    let id = id.to_string_lossy().trim_end_matches(".jsonl").to_string();
    let out = env.run(&["learn", "--session", &id]);
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(env.requests().len(), 2, "the gate skipped the LLM review");
    let log = std::fs::read_to_string(env.home.join("learn.log")).unwrap();
    assert!(log.contains("gate local") && log.contains("-> None"), "{log}");
}
