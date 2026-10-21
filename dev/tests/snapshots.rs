//! Golden snapshots of what the model sees and what the user sees.
//!
//! One test per shape of first request (plain turn, routed skill, routed web,
//! routed MCP, learning review) and two screens (first frame, finished turn).
//! A refactor that changes none of these is behaviour-neutral as far as the
//! model and the user can tell; a prompt or schema edit shows up as a diff.
//!
//! Never edit a `.snap` file by hand: run the tests, read the diff, and if the
//! change is intended accept it with `cargo insta accept` (or
//! `INSTA_UPDATE=always cargo test -p construct-dev --test snapshots`) and say
//! so in the PR.

mod common;

use common::*;
use construct_dev::drive;
use serde_json::{Value, json};

/// The plain first request must stay under this many bytes (AGENTS.md: keep
/// the first request small).
const PLAIN_LIMIT: usize = 6000;

/// Replace what legitimately varies between runs and machines: temp paths,
/// session ids, the date and os lines of the system prompt.
fn normalize(env: &Env, s: &str) -> String {
    let mut s = s.replace(&env.work.display().to_string(), "[WORK]");
    s = s.replace(&env.home.display().to_string(), "[HOME]");
    s = s.replace(&env.dir.display().to_string(), "[DIR]");
    s = s.replace(&format!("127.0.0.1:{}", env.port), "127.0.0.1:[PORT]");
    if env.home.join("sessions").exists() {
        for (i, id) in env.sessions().iter().enumerate() {
            s = s.replace(id, &format!("[SESSION{i}]"));
        }
    }
    // `- date: YYYY-MM-DD`: keep the label, drop the ten date characters.
    let mut parts = s.split("- date: ");
    let mut out = parts.next().unwrap_or_default().to_string();
    for part in parts {
        out.push_str("- date: [DATE]");
        out.push_str(part.get(10..).unwrap_or_default());
    }
    out.replace(&format!("- os: {} {}", std::env::consts::OS, std::env::consts::ARCH), "- os: [OS]")
}

/// The request wrapped with its size, so a size change is visible in review
/// even when the content diff is long.
fn shaped(env: &Env, req: &Value) -> Value {
    let req: Value = serde_json::from_str(&normalize(env, &req.to_string())).unwrap();
    json!({
        "bytes": serde_json::to_vec(&req).unwrap().len(),
        "n_tools": tool_names(&req).len(),
        "request": req,
    })
}

fn first_request(name: &str, config: &str, prompt: &str, setup: impl FnOnce(&Env)) -> (Env, Value) {
    let env = Env::new(name, json!({ "steps": [ { "text": "ok" } ] }));
    env.config(config);
    setup(&env);
    let out = env.run(&["run", "--no-learn", prompt]);
    assert!(out.status.success(), "{}", text(&out));
    let req = env.requests().into_iter().next().expect("a request");
    (env, req)
}

#[test]
fn plain_turn() {
    let (env, req) =
        first_request("snap-plain", "[router]\nmode = \"local\"\n", "explain what this function does", |_| {});
    let snap = shaped(&env, &req);
    let bytes = snap["bytes"].as_u64().unwrap() as usize;
    assert!(bytes < PLAIN_LIMIT, "plain first request is {bytes} bytes (limit {PLAIN_LIMIT})");
    insta::assert_json_snapshot!(snap);
}

#[test]
fn routed_skill() {
    let (env, req) = first_request(
        "snap-skill",
        "[router]\nmode = \"local\"\n",
        "publish a new release of this crate with a changelog",
        |env| {
            let skill = env.home.join("skills/release-crate/SKILL.md");
            std::fs::create_dir_all(skill.parent().unwrap()).unwrap();
            std::fs::write(
                &skill,
                "---\nname: release-crate\ndescription: Publish a release of a Rust crate: bump version, changelog, tag\n---\n\n1. bump\n",
            )
            .unwrap();
        },
    );
    insta::assert_json_snapshot!(shaped(&env, &req));
}

#[test]
fn routed_web() {
    let (env, req) =
        first_request("snap-web", "[router]\nmode = \"local\"\n", "summarize https://example.com/changelog", |_| {});
    insta::assert_json_snapshot!(shaped(&env, &req));
}

#[test]
fn routed_mcp() {
    let config = format!(
        "[router]\nmode = \"local\"\n[mcp.calc]\ncommand = \"{}\"\nargs = [\"mcp-fake\"]\ndescription = \"calculator arithmetic add numbers\"\n",
        dev_bin().display()
    );
    let (env, req) = first_request("snap-mcp", &config, "use the calc server to add 2 and 3", |_| {});
    // The dev binary's path is machine-specific and isn't sent to the model.
    insta::assert_json_snapshot!(shaped(&env, &req));
}

#[test]
fn learning_review() {
    let obs = json!({ "observations": [{
        "problem_class": "missing release output directory",
        "summary": "release dir was missing",
        "fix": "mkdir -p dist first",
        "evidence": "ls: cannot access '/nonexistent-release-dir'"
    }] })
    .to_string();
    let env = Env::new(
        "snap-learn",
        json!({ "steps": [
            { "call": "shell", "args": { "command": "ls /nonexistent-release-dir" } },
            { "call": "shell", "args": { "command": "exit 2" } },
            { "call": "shell", "args": { "command": "false" } },
            { "text": "released" },
            { "text": "Nothing to save." }
        ], "side": [obs] }),
    );
    let out = env.run(&["run", "--no-learn", "release the crate. I always do this on Fridays."]);
    assert!(out.status.success(), "{}", text(&out));
    let out = env.run(&["learn", "--session", &env.sessions()[0]]);
    assert!(out.status.success(), "{}", text(&out));
    let review = env
        .requests()
        .into_iter()
        .find(|r| r["messages"][0]["content"].as_str().unwrap_or("").contains("learning pass"))
        .expect("review request");
    insta::assert_json_snapshot!(shaped(&env, &review));
}

/// Screen text with trailing spaces trimmed, durations masked and the env's
/// paths normalised.
fn screen(env: &Env, d: &drive::Driver) -> String {
    let text = d.text();
    let lines: Vec<String> = text
        .lines()
        .map(|l| l.trim_end().split(' ').map(|w| if is_duration(w) { "[T]" } else { w }).collect::<Vec<_>>().join(" "))
        .collect();
    normalize(env, &lines.join("\n"))
}

/// `4ms`, `0.0s`, `12s`: timings that vary run to run.
fn is_duration(w: &str) -> bool {
    let num = w.strip_suffix("ms").or_else(|| w.strip_suffix('s')).unwrap_or("");
    !num.is_empty()
        && num.chars().all(|c| c.is_ascii_digit() || c == '.')
        && num.starts_with(|c: char| c.is_ascii_digit())
}

#[test]
fn screens() {
    let env = Env::new(
        "snap-tui",
        json!({ "steps": [
            { "call": "shell", "args": { "command": "echo from-the-shell" } },
            { "text": "## Done\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\nAll **good**." }
        ] }),
    );
    let mut d = drive::Driver::spawn(
        &[construct().display().to_string()],
        &env.env(),
        Some(env.work.to_str().unwrap()),
        100,
        30,
    )
    .unwrap();
    let mut log = Vec::new();
    let (short, long) = (budget(3000), budget(5000));
    let run = |d: &mut drive::Driver, log: &mut Vec<u8>, script: &str| {
        if let Err(e) = drive::run_script(d, script, log) {
            panic!("{e:#}\n{}", String::from_utf8_lossy(log));
        }
    };
    run(&mut d, &mut log, &format!("wait \"ask construct\" {short}\nsettle 300 {long}\n"));
    insta::assert_snapshot!("first_frame", screen(&env, &d));
    run(&mut d, &mut log, &format!("type run it\nkey enter\nwait \"All good.\" {long}\nsettle 300 {long}\n"));
    insta::assert_snapshot!("finished_turn", screen(&env, &d));
    d.kill();
}
