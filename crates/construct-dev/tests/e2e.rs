//! End-to-end tests: the real `construct` binary, a fresh home per test, and
//! a scripted model endpoint. Each test checks what a user (or the model)
//! would observe: files on disk, what the harness sent, what the screen shows.

mod common;

use common::*;
use construct_dev::drive;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

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
    assert_eq!(
        names,
        ["shell", "read", "write", "edit", "grep", "glob", "enable_tools"],
        "plain turn gets only core tools"
    );
    let bytes = serde_json::to_vec(first).unwrap().len();
    assert!(bytes < 6000, "first request is {bytes} bytes");

    let out = env.run(&["run", "--no-learn", "publish a new release of this crate with a changelog"]);
    assert!(out.status.success());
    let req = &env.requests()[1];
    assert!(tool_names(req).contains(&"skill".to_string()));
    let user = req["messages"].as_array().unwrap().last().unwrap()["content"].as_str().unwrap().to_string();
    assert!(user.contains("release-crate"), "skill suggested on the user turn: {user}");
    assert!(
        !req["messages"][0]["content"].as_str().unwrap().contains("release-crate"),
        "no skill index in the system prompt"
    );

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
fn hashline_rejects_a_stale_anchor_with_a_window_and_the_retry_lands() {
    // Anchors are pinned: they are the FNV-1a hashes the TypeScript reference
    // gives these lines, before and after "inserted" lands on top.
    let env = Env::new(
        "hashline",
        json!({ "steps": [
            { "call": "read", "args": { "path": "notes.txt" } },
            { "call": "shell", "args": { "command": "sed -i '1i inserted' notes.txt" } },
            { "call": "edit", "args": { "path": "notes.txt", "edits": [ { "op": "set", "anchor": "2:ds", "text": "BETA" } ] } },
            { "call": "edit", "args": { "path": "notes.txt", "edits": [ { "op": "set", "anchor": "3:ds", "text": "BETA" } ] } },
            { "text": "done" }
        ] }),
    );
    std::fs::write(env.work.join("notes.txt"), "alpha\nbeta\ngamma\ndelta\n").unwrap();
    let out =
        env.run_with(&[("CONSTRUCT_TOOLS_EDITOR", "hashline")], &["run", "--json", "--no-learn", "capitalise beta"]);
    assert!(out.status.success(), "{}", text(&out));
    let reqs = env.requests();
    assert_eq!(tool_names(&reqs[0]), ["shell", "read", "write", "edit", "grep", "glob", "enable_tools"]);
    let edit = reqs[0]["tools"].as_array().unwrap().iter().find(|t| t["function"]["name"] == "edit").unwrap();
    assert!(edit["function"]["parameters"]["properties"]["edits"].is_object(), "{edit}");
    assert_eq!(last_tool_result(&reqs[1]), "1:jk|alpha\n2:ds|beta\n3:ab|gamma\n4:ld|delta\n");
    // The file moved under the model: the old anchor is refused, nothing is
    // written, and the window carries the line's new anchor.
    let rejected = last_tool_result(&reqs[3]);
    assert!(rejected.starts_with("Error: REJECTED: 1 of 1 edit(s)"), "{rejected}");
    assert!(rejected.contains("ANCHOR_MISMATCH") && rejected.contains("3:ds|beta"), "{rejected}");
    let applied = last_tool_result(&reqs[4]);
    assert!(applied.starts_with("ok: applied 1 edit(s) to notes.txt: replaced line 3"), "{applied}");
    assert!(applied.lines().any(|l| l.starts_with("3:") && l.ends_with("|BETA")), "fresh anchors: {applied}");
    assert_eq!(std::fs::read_to_string(env.work.join("notes.txt")).unwrap(), "inserted\nalpha\nBETA\ngamma\ndelta\n");
    // Scorers read outcomes from events, not prose.
    let events: Vec<Value> =
        String::from_utf8_lossy(&out.stdout).lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
    let edits: Vec<&Value> = events.iter().filter(|e| e["event"] == "tool_end" && e["name"] == "edit").collect();
    assert_eq!((edits[0]["ok"].clone(), edits[0]["code"].clone()), (json!(false), json!("ANCHOR_MISMATCH")));
    assert_eq!(edits[0]["data"]["errors"][0]["code"], "ANCHOR_MISMATCH");
    assert_eq!((edits[1]["ok"].clone(), edits[1]["data"]["status"].clone()), (json!(true), json!("applied")));
    let manifest = events.iter().find(|e| e["event"] == "manifest").expect("manifest event");
    assert_eq!(manifest["editor"], "hashline");
    assert_eq!(manifest["hashline"]["anchor"], "line+hash");
    // The session log keeps the same record for the bench, joined to the call by id.
    let log = std::fs::read_to_string(env.home.join("sessions").join(format!("{}.jsonl", env.sessions()[0]))).unwrap();
    let logged: Vec<Value> = log
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|v| v["type"] == "tool" && v["data"]["name"] == "edit")
        .map(|v| v["data"].clone())
        .collect();
    assert_eq!(logged.len(), 2, "{log}");
    assert_eq!((logged[0]["ok"].clone(), logged[0]["code"].clone()), (json!(false), json!("ANCHOR_MISMATCH")));
    assert_eq!((logged[1]["ok"].clone(), logged[1]["data"]["status"].clone()), (json!(true), json!("applied")));
    assert_eq!(logged[0]["id"], edits[0]["id"]);
}

#[test]
fn hashline_hash_mode_follows_moved_content() {
    // The same shift as above, but the bare hash names content wherever it is.
    let env = Env::new(
        "hashline-hash",
        json!({ "steps": [
            { "call": "read", "args": { "path": "notes.txt" } },
            { "call": "shell", "args": { "command": "sed -i '1i inserted' notes.txt" } },
            { "call": "edit", "args": { "path": "notes.txt", "edits": [ { "op": "set", "anchor": "ds", "text": "BETA" } ] } },
            { "text": "done" }
        ] }),
    );
    env.config("[tools]\neditor = \"hashline\"\n\n[tools.hashline]\nanchor = \"hash\"\n");
    std::fs::write(env.work.join("notes.txt"), "alpha\nbeta\ngamma\ndelta\n").unwrap();
    let out = env.run(&["run", "--json", "--no-learn", "capitalise beta"]);
    assert!(out.status.success(), "{}", text(&out));
    let reqs = env.requests();
    assert_eq!(last_tool_result(&reqs[1]), "jk|alpha\nds|beta\nab|gamma\nld|delta\n");
    assert!(last_tool_result(&reqs[3]).starts_with("ok: applied 1 edit(s) to notes.txt: replaced line 3"));
    assert_eq!(std::fs::read_to_string(env.work.join("notes.txt")).unwrap(), "inserted\nalpha\nBETA\ngamma\ndelta\n");
    let manifest: Value = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find(|e| e["event"] == "manifest")
        .expect("manifest event");
    assert_eq!(manifest["hashline"]["anchor"], "hash");
}

#[test]
fn hashline_count_salt_rejects_a_stale_duplicate_and_the_retry_lands() {
    // Issue #26's shape: a block holding another `return None` lands above
    // the final one. Under the rank salt the stale `gg` would name the banned
    // branch's return; the count salt re-hashes every copy, so it is gone.
    let rewrite = "printf 'def f(u):\\n    if u.admin:\\n        return None\\n    if u.banned:\\n        return None\\n    audit(u)\\n    return None\\n' > f.py";
    let env = Env::new(
        "hashline-count",
        json!({ "steps": [
            { "call": "read", "args": { "path": "f.py" } },
            { "call": "shell", "args": { "command": rewrite } },
            { "call": "edit", "args": { "path": "f.py", "edits": [ { "op": "set", "anchor": "gg", "text": "    return u" } ] } },
            { "call": "edit", "args": { "path": "f.py", "edits": [ { "op": "set", "anchor": "yf", "text": "    return u" } ] } },
            { "text": "done" }
        ] }),
    );
    env.config("[tools]\neditor = \"hashline\"\n\n[tools.hashline]\nanchor = \"hash\"\nsalt = \"count\"\n");
    std::fs::write(
        env.work.join("f.py"),
        "def f(u):\n    if u.banned:\n        return None\n    audit(u)\n    return None\n",
    )
    .unwrap();
    let out = env.run(&["run", "--json", "--no-learn", "return the user at the end"]);
    assert!(out.status.success(), "{}", text(&out));
    let reqs = env.requests();
    assert!(last_tool_result(&reqs[1]).ends_with("gg|    return None\n"), "{}", last_tool_result(&reqs[1]));
    let rejected = last_tool_result(&reqs[3]);
    assert!(
        rejected.starts_with("Error: REJECTED: 1 of 1 edit(s)") && rejected.contains("ANCHOR_NOT_FOUND"),
        "{rejected}"
    );
    assert!(last_tool_result(&reqs[4]).starts_with("ok: applied 1 edit(s) to f.py: replaced line 7"));
    assert_eq!(
        std::fs::read_to_string(env.work.join("f.py")).unwrap(),
        "def f(u):\n    if u.admin:\n        return None\n    if u.banned:\n        return None\n    audit(u)\n    return u\n"
    );
    let manifest: Value = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find(|e| e["event"] == "manifest")
        .expect("manifest event");
    assert_eq!(
        (manifest["hashline"]["anchor"].clone(), manifest["hashline"]["salt"].clone()),
        (json!("hash"), json!("count"))
    );
}

#[test]
fn disabled_tools_do_not_exist_for_the_session_or_the_learning_pass() {
    let obs = json!({ "observations": [{ "problem_class": "missing release output directory", "summary": "release dir was missing", "fix": "mkdir -p dist first", "evidence": "ls: cannot access '/nonexistent-release-dir'" }] }).to_string();
    let env = Env::new(
        "disabled",
        json!({ "steps": [
            { "call": "write", "args": { "path": "x.txt", "content": "hi" } },
            { "call": "memory", "args": { "target": "user", "operations": [ { "action": "add", "content": "Likes tea" } ] } },
            { "call": "shell", "args": { "command": "ls /nonexistent-release-dir" } },
            { "call": "shell", "args": { "command": "exit 2" } },
            { "call": "shell", "args": { "command": "false" } },
            { "text": "done" },
            { "text": "Nothing to save." }
        ], "side": [obs] }),
    );
    env.config(
        "[router]\nmode = \"local\"\n\n[tools]\ndisabled = [\"write\", \"memory\", \"web_search\", \"web_fetch\"]\n",
    );
    let out = env.run(&["run", "--json", "--no-learn", "release the crate. I always do this on Fridays."]);
    assert!(out.status.success(), "{}", text(&out));
    let reqs = env.requests();
    // Absent from definitions, and from what enable_tools offers.
    assert_eq!(tool_names(&reqs[0]), ["shell", "read", "edit", "grep", "glob", "enable_tools"]);
    let enable = reqs[0]["tools"].as_array().unwrap().iter().find(|t| t["function"]["name"] == "enable_tools").unwrap();
    let offer = enable["function"]["parameters"]["properties"]["groups"]["items"]["enum"].to_string();
    assert!(!offer.contains("memory") && !offer.contains("web"), "{offer}");
    // "unknown tool" when called anyway, and nothing happened.
    assert_eq!(last_tool_result(&reqs[1]), "Error: unknown tool write");
    assert_eq!(last_tool_result(&reqs[2]), "Error: unknown tool memory");
    assert!(!tool_names(&reqs[2]).contains(&"memory".to_string()), "calling it didn't switch its group on");
    assert!(!env.work.join("x.txt").exists());
    assert!(!env.home.join("memory/USER.md").exists());
    // Events and the session log carry the code and the manifest.
    let events: Vec<Value> =
        String::from_utf8_lossy(&out.stdout).lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
    let end = events.iter().find(|e| e["event"] == "tool_end" && e["name"] == "write").expect("tool_end");
    assert_eq!((end["ok"].clone(), end["code"].clone()), (json!(false), json!("unknown_tool")));
    let shell_end = events.iter().find(|e| e["event"] == "tool_end" && e["name"] == "shell").unwrap();
    assert_eq!((shell_end["code"].clone(), shell_end["kind"].clone()), (json!("exit"), json!("tail")));
    let manifest = events.iter().find(|e| e["event"] == "manifest").expect("manifest event");
    assert!(!manifest["tools"].to_string().contains("\"write\""), "{manifest}");
    assert_eq!(manifest["editor"], "replace");
    let log = std::fs::read_to_string(env.home.join("sessions").join(format!("{}.jsonl", env.sessions()[0]))).unwrap();
    let header: Value = serde_json::from_str(log.lines().next().unwrap()).unwrap();
    assert_eq!(header["manifest"]["tools"], manifest["tools"]);
    // The learning pass builds its tools from the same config.
    let out = env.run(&["learn", "--session", &env.sessions()[0]]);
    assert!(out.status.success(), "{}", text(&out));
    let review = env
        .requests()
        .into_iter()
        .find(|r| r["messages"][0]["content"].as_str().unwrap_or("").contains("learning pass"))
        .expect("review request");
    assert!(!tool_names(&review).contains(&"memory".to_string()), "{:?}", tool_names(&review));
}

#[test]
fn learning_records_observations_and_writes_a_skill_only_on_recurrence() {
    let skill_md = "---\nname: crate-release\ndescription: Releasing a Rust crate when the release dir is missing\n---\n\n1. mkdir -p dist\n2. cargo package\n";
    let big = "x".repeat(1500);
    let obs = |s: &str| {
        json!({ "observations": [{ "problem_class": "missing release output directory", "summary": s, "fix": "mkdir -p dist first", "evidence": "ls: cannot access '/nonexistent-release-dir'" }] }).to_string()
    };
    let env = Env::new(
        "learn",
        json!({ "steps": [
            // session 1
            { "call": "shell", "args": { "command": "ls /nonexistent-release-dir" } },
            { "call": "shell", "args": { "command": "exit 2" } },
            { "call": "shell", "args": { "command": "false" } },
            { "text": "released" },
            // its review: memory only, no skill yet
            { "call": "memory", "args": { "target": "user", "operations": [ { "action": "add", "content": big } ] } },
            { "call": "memory", "args": { "target": "user", "operations": [ { "action": "add", "content": "Releases crates on Fridays" } ] } },
            { "text": "Saved a preference." },
            // session 2: the same landmine
            { "call": "shell", "args": { "command": "ls /nonexistent-release-dir" } },
            { "call": "shell", "args": { "command": "exit 2" } },
            { "call": "shell", "args": { "command": "false" } },
            { "text": "released again" },
            // its review: now it has recurred
            { "call": "skill_manage", "args": { "action": "create", "name": "crate-release", "content": skill_md } },
            { "text": "Wrote a release skill." }
        ], "side": [obs("release dir was missing"), obs("release dir missing again")] }),
    );
    env.config("[memory]\nuser_chars = 1000\n");
    let session = |n: usize| {
        // Ids have one-second resolution; order by when the session file appeared.
        let mut ids: Vec<(std::time::SystemTime, String)> = std::fs::read_dir(env.home.join("sessions"))
            .unwrap()
            .flatten()
            .map(|e| {
                (
                    e.metadata().unwrap().created().or_else(|_| e.metadata().unwrap().modified()).unwrap(),
                    e.file_name().to_string_lossy().trim_end_matches(".jsonl").to_string(),
                )
            })
            .collect();
        ids.sort();
        ids[n].1.clone()
    };

    let out = env.run(&["run", "--no-learn", "release the crate. I always do this on Fridays."]);
    assert!(out.status.success(), "{}", text(&out));
    let out = env.run(&["learn", "--session", &session(0)]);
    assert!(out.status.success(), "{}", text(&out));
    let is_review = |r: &&Value| r["messages"][0]["content"].as_str().unwrap_or("").contains("learning pass");
    let reqs = env.requests();
    let review = reqs.iter().find(is_review).expect("review request");
    let names = tool_names(review);
    assert!(!names.contains(&"skill_manage".into()), "one session doesn't earn a skill: {names:?}");
    assert!(!names.contains(&"shell".into()), "the learning pass can only touch memory and skills");
    assert!(review["messages"][1]["content"].as_str().unwrap().contains("USER: release the crate"));
    assert!(reqs.iter().any(|r| last_tool_result(r).contains("over the cap")), "over-cap write refused and explained");
    assert_eq!(std::fs::read_to_string(env.home.join("memory/USER.md")).unwrap(), "Releases crates on Fridays");
    assert!(!env.home.join("skills/crate-release").exists());
    let ledger = std::fs::read_to_string(env.home.join("learn/ledger.jsonl")).unwrap();
    assert_eq!(ledger.lines().count(), 1);
    assert!(ledger.contains("missing release output directory"));

    let out = env.run(&["run", "--no-learn", "release the crate"]);
    assert!(out.status.success(), "{}", text(&out));
    let out = env.run(&["learn", "--session", &session(1)]);
    assert!(out.status.success(), "{}", text(&out));
    let reqs = env.requests();
    let review = reqs
        .iter()
        .rev()
        .filter(is_review)
        .find(|r| tool_names(r).contains(&"skill_manage".to_string()))
        .expect("skill review after recurrence");
    let prompt = review["messages"][1]["content"].as_str().unwrap();
    assert!(prompt.contains("RECURRING PROBLEMS"), "{prompt}");
    assert!(prompt.contains(&session(0)) && prompt.contains(&session(1)), "evidence from both sessions");
    assert!(std::fs::read_to_string(env.home.join("skills/crate-release/SKILL.md")).unwrap().contains("mkdir -p dist"));
    let usage: Value =
        serde_json::from_str(&std::fs::read_to_string(env.home.join("skills/.usage.json")).unwrap()).unwrap();
    assert_eq!(usage["crate-release"]["author"], "agent");
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
    let script = format!(
        r#"
wait "ask construct" {short}
type run it
key enter
wait "from-the-shell" {long}
wait "All good." {long}
expect "│ 2"
type slow one
key enter
wait "thinking" {short}
key esc
wait "interrupted" {short}
type again
key enter
wait "back again" {long}
"#
    );
    let r = drive::run_script(&mut d, &script, &mut log);
    if let Err(e) = &r {
        panic!("{e:#}\n{}", String::from_utf8_lossy(&log));
    }
    let m = r.unwrap();
    let first = m["first_frame_ms"].as_f64().unwrap();
    let limit = budget(500) as f64;
    assert!(first < limit, "first frame took {first}ms (limit {limit}ms)");
    d.send(b"/quit\r").unwrap();
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        d.exited() || {
            d.pump(Duration::from_millis(budget(500)));
            d.exited()
        },
        "quit exits"
    );
}

#[test]
fn a_session_with_nothing_to_learn_costs_no_review() {
    let env = Env::new(
        "gate",
        json!({ "steps": [ { "call": "read", "args": { "path": "a.txt" } }, { "text": "It says hello." } ] }),
    );
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

#[test]
fn jev_down_falls_back_to_local_routing_and_is_logged() {
    let env = Env::new("jevdown", json!({ "steps": [{ "text": "Here's the latest." }] }));
    // Jev "lives" on a path the scripted endpoint answers with 404.
    env.config(&format!("[router]\njev_url = \"http://127.0.0.1:{}/v1/systemone\"\n", env.port));
    let out = env.run_with(
        &[("TYPESAFE_API_KEY", "test")],
        &["run", "--json", "--no-learn", "search the web for the latest tokio release"],
    );
    assert!(out.status.success(), "{}", text(&out));
    let route: Value = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find(|e| e["event"] == "route")
        .expect("route event");
    assert_eq!(route["source"], "local");
    assert!(route["note"].as_str().unwrap().contains("jev failed"), "{route}");
    // The local router still caught the web request.
    assert!(tool_names(&env.requests()[0]).contains(&"web_search".to_string()));
    let log = std::fs::read_to_string(env.home.join("jev.log")).expect("jev.log");
    let call: Value = serde_json::from_str(log.lines().next().unwrap()).unwrap();
    assert_eq!(call["purpose"], "route");
    assert_eq!(call["ok"], false);
}

#[test]
fn sandbox_keeps_tools_inside_the_workspace() {
    let env = Env::new(
        "sandbox",
        json!({ "steps": [
            { "call": "shell", "args": { "command": "cat \"$HOME/.config/fake.key\"" } },
            { "call": "shell", "args": { "command": "cat ../outside/secret.txt" } },
            { "call": "shell", "args": { "command": "env" } },
            { "call": "shell", "args": { "command": "tr '\\0' '\\n' < /proc/$(cut -d' ' -f4 /proc/$PPID/stat)/environ" } },
            { "call": "read", "args": { "path": "../outside/secret.txt" } },
            { "call": "read", "args": { "path": "link.txt" } },
            { "call": "write", "args": { "path": "../outside/new.txt", "content": "x" } },
            { "call": "shell", "args": { "command": "echo ok > made.txt && cat made.txt" } },
            { "call": "shell", "args": { "command": "touch /dev/shm/construct-e2e-probe && echo SHM-WRITABLE; echo hi > /dev/null && echo devnull-ok" } },
            { "text": "done" }
        ] }),
    );
    let fake_home = env.dir.join("fakehome");
    std::fs::create_dir_all(fake_home.join(".config")).unwrap();
    std::fs::write(fake_home.join(".config/fake.key"), "SECRET-IN-HOME").unwrap();
    std::fs::create_dir_all(env.dir.join("outside")).unwrap();
    std::fs::write(env.dir.join("outside/secret.txt"), "SECRET-OUTSIDE").unwrap();
    std::os::unix::fs::symlink(env.dir.join("outside/secret.txt"), env.work.join("link.txt")).unwrap();
    let home = fake_home.display().to_string();
    let out =
        env.run_with(&[("HOME", &home), ("FAKE_API_KEY", "SECRET-IN-ENV")], &["run", "--no-learn", "poke around"]);
    assert!(out.status.success(), "{}", text(&out));
    let last = env.requests().pop().unwrap();
    let results: Vec<String> = last["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["role"] == "tool")
        .map(|m| m["content"].as_str().unwrap_or("").to_string())
        .collect();
    assert_eq!(results.len(), 9, "{results:#?}");
    for (i, r) in results.iter().enumerate() {
        assert!(!r.contains("SECRET-"), "step {i} leaked: {r}");
    }
    assert!(
        !results[2].contains("OPENAI_API_KEY") && !results[2].contains("FAKE_API_KEY"),
        "credentials scrubbed from env: {}",
        results[2]
    );
    assert!(results[2].contains("PATH="), "env still works: {}", results[2]);
    assert!(results[4].contains("blocked by the sandbox"), "{}", results[4]);
    assert!(results[5].contains("blocked by the sandbox"), "symlink escape: {}", results[5]);
    assert!(results[6].contains("blocked by the sandbox"), "{}", results[6]);
    assert!(!env.dir.join("outside/new.txt").exists());
    assert_eq!(results[7].trim(), "ok", "{}", results[7]);
    assert_eq!(std::fs::read_to_string(env.work.join("made.txt")).unwrap(), "ok\n");
    assert!(!results[8].contains("SHM-WRITABLE") && results[8].contains("devnull-ok"), "{}", results[8]);
}

/// Whether a `sleep 300` started by a test command is still running.
fn sleep_alive(work: &std::path::Path, pidfile: &str) -> bool {
    let pid = std::fs::read_to_string(work.join(pidfile)).unwrap_or_else(|_| panic!("{pidfile} not written"));
    std::fs::read(format!("/proc/{}/cmdline", pid.trim())).is_ok_and(|c| c == b"sleep\x00300\x00")
}

fn tool_results(env: &Env) -> Vec<String> {
    let last = env.requests().pop().unwrap();
    last["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["role"] == "tool")
        .map(|m| m["content"].as_str().unwrap_or("").to_string())
        .collect()
}

fn escape_steps() -> Value {
    // setsid and nohup exec in place, so the first command waits until $! is
    // `sleep` before ending: the sweep names whatever the process is by then.
    json!({ "steps": [
        { "call": "shell", "args": { "command": "setsid nohup sleep 300 >/dev/null 2>&1 & echo $! > a.pid; for i in $(seq 100); do [ \"$(cat /proc/$!/comm)\" = sleep ] && break; sleep 0.05; done" } },
        { "call": "shell", "args": { "command": "sleep 300 & echo $! > b.pid" } },
        { "call": "shell", "args": { "command": "setsid sleep 300 & echo $! > c.pid; sleep 30", "timeout": 1 } },
        { "text": "done" }
    ] })
}

fn assert_nothing_escaped(env: &Env, mode: &[(&str, &str)]) {
    let t = std::time::Instant::now();
    let out = env.run_with(mode, &["run", "--no-learn", "start things"]);
    assert!(out.status.success(), "{}", text(&out));
    // A background job holding stdout no longer holds the call open, and the
    // timed-out call ends at its timeout, not at the sleeps'.
    assert!(t.elapsed() < Duration::from_millis(budget(8000)), "took {:?}", t.elapsed());
    let r = tool_results(env);
    assert_eq!(r.len(), 3, "{r:#?}");
    assert!(r[0].contains("[stopped 1 background process: sleep]"), "{}", r[0]);
    assert!(r[1].contains("[stopped 1 background process"), "{}", r[1]);
    assert!(r[2].contains("timed out") && r[2].contains("background process"), "{}", r[2]);
    for f in ["a.pid", "b.pid", "c.pid"] {
        assert!(!sleep_alive(&env.work, f), "{f} outlived its call");
    }
}

#[test]
fn shell_calls_leave_nothing_running() {
    let env = Env::new("escape", escape_steps());
    assert_nothing_escaped(&env, &[]);
}

#[test]
fn shell_calls_leave_nothing_running_with_the_sandbox_off() {
    let env = Env::new("escape-off", escape_steps());
    assert_nothing_escaped(&env, &[("CONSTRUCT_SANDBOX", "off")]);
}

#[test]
fn killing_construct_mid_call_stops_the_command_tree() {
    let env = Env::new(
        "escape-kill",
        json!({ "steps": [
            { "call": "shell", "args": { "command": "setsid sleep 300 & echo $! > a.pid; sleep 30" } },
            { "text": "done" }
        ] }),
    );
    let mut c = Command::new(construct());
    c.args(["run", "--no-learn", "start things"]).current_dir(&env.work);
    for (k, v) in env.env() {
        if v.is_empty() {
            c.env_remove(k);
        } else {
            c.env(k, v);
        }
    }
    let mut child = c.stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn().unwrap();
    let pidfile = env.work.join("a.pid");
    let t = std::time::Instant::now();
    while !pidfile.exists() && t.elapsed() < Duration::from_millis(budget(5000)) {
        std::thread::sleep(Duration::from_millis(20));
    }
    std::thread::sleep(Duration::from_millis(50));
    assert!(sleep_alive(&env.work, "a.pid"), "the command didn't start");
    child.kill().unwrap();
    child.wait().unwrap();
    let t = std::time::Instant::now();
    while sleep_alive(&env.work, "a.pid") && t.elapsed() < Duration::from_millis(budget(3000)) {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!sleep_alive(&env.work, "a.pid"), "the escaped sleep outlived construct");
}

#[test]
fn project_kit_follows_the_repo_not_the_session() {
    let skill_md = "---\nname: run-tests-here\ndescription: Running this repo's tests (fixtures must be generated first)\n---\n\n1. ./tools/gen-fixtures\n2. python3 -m unittest\n";
    let env = Env::new(
        "project",
        json!({ "steps": [
            { "call": "memory", "args": { "target": "project", "operations": [ { "action": "add", "content": "Tests need ./tools/gen-fixtures first" } ] } },
            { "call": "skill_manage", "args": { "action": "create", "scope": "project", "name": "run-tests-here", "content": skill_md } },
            { "text": "noted" },
            { "text": "hi from repo a" },
            { "text": "hi from the clone" },
            { "text": "hi from repo b" },
            { "call": "memory", "args": { "target": "project", "operations": [ { "action": "add", "content": "x" } ] } },
            { "text": "done" }
        ] }),
    );
    let repo_a = env.work.join("a");
    let repo_b = env.work.join("b");
    for r in [&repo_a, &repo_b] {
        std::fs::create_dir_all(r.join(".git")).unwrap();
        std::fs::write(r.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
    }
    // An empty .git directory (as found in /tmp) is not a repo.
    std::fs::create_dir_all(env.work.join(".git")).unwrap();
    std::fs::write(repo_a.join(".git/config"), "[remote \"origin\"]\n\turl = git@example.com:me/a.git\n").unwrap();
    let in_dir = |dir: &PathBuf, args: &[&str]| {
        let mut c = Command::new(construct());
        c.args(args).current_dir(dir).env_remove("TYPESAFE_API_KEY");
        for (k, v) in env.env() {
            if !v.is_empty() {
                c.env(k, v);
            }
        }
        c.output().unwrap()
    };
    // Remember a fact and a procedure for repo a.
    let out = in_dir(&repo_a.join("."), &["run", "--no-learn", "remember how tests work here"]);
    assert!(out.status.success(), "{}", text(&out));
    let kits: Vec<_> = std::fs::read_dir(env.home.join("projects")).unwrap().flatten().map(|e| e.path()).collect();
    assert_eq!(kits.len(), 1);
    let kit = &kits[0];
    assert!(kit.file_name().unwrap().to_string_lossy().starts_with("a-"));
    assert_eq!(std::fs::read_to_string(kit.join("NOTES.md")).unwrap(), "Tests need ./tools/gen-fixtures first");
    assert!(kit.join("skills/run-tests-here/SKILL.md").exists());
    assert!(!env.home.join("skills/run-tests-here").exists(), "project skill stays out of the global library");
    assert!(std::fs::read_to_string(kit.join("project.json")).unwrap().contains("example.com/me/a"));
    // A new session in a subdirectory of repo a starts with both.
    std::fs::create_dir_all(repo_a.join("src")).unwrap();
    let out = in_dir(&repo_a.join("src"), &["run", "--no-learn", "hi"]);
    assert!(out.status.success(), "{}", text(&out));
    let sys = env.requests().last().unwrap()["messages"][0]["content"].as_str().unwrap().to_string();
    assert!(
        sys.contains("Notes on this project (a)") && sys.contains("Tests need ./tools/gen-fixtures first"),
        "{sys}"
    );
    assert!(sys.contains("run-tests-here: Running this repo's tests"), "{sys}");
    // A second clone of the same remote, in a differently named directory, shares the kit.
    let clone = env.work.join("a-clone");
    std::fs::create_dir_all(clone.join(".git")).unwrap();
    std::fs::write(clone.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
    std::fs::write(clone.join(".git/config"), "[remote \"origin\"]\n\turl = https://example.com/me/a\n").unwrap();
    let out = in_dir(&clone, &["run", "--no-learn", "hi"]);
    assert!(out.status.success(), "{}", text(&out));
    let sys = env.requests().last().unwrap()["messages"][0]["content"].as_str().unwrap().to_string();
    assert!(sys.contains("Tests need ./tools/gen-fixtures first"), "clone shares the kit: {sys}");
    // Repo b sees none of it.
    let out = in_dir(&repo_b, &["run", "--no-learn", "hi"]);
    assert!(out.status.success(), "{}", text(&out));
    let sys = env.requests().last().unwrap()["messages"][0]["content"].as_str().unwrap().to_string();
    assert!(!sys.contains("gen-fixtures") && !sys.contains("run-tests-here"), "{sys}");
    // Outside any repo there is no project to write to.
    let out = in_dir(&env.work, &["run", "--no-learn", "note this"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(last_tool_result(env.requests().last().unwrap()).contains("not in a git repo"));
}
