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
    // One session in the journal, opened by its head.
    let rows = env.journal();
    assert_eq!(env.sessions().len(), 1);
    assert_eq!((rows[0].ty.as_str(), rows[0].payload["harness"].clone()), ("head", json!("construct")));
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
    // The journal keeps the same record for the bench, joined to the call by id.
    let logged: Vec<Value> = env
        .journal()
        .into_iter()
        .filter(|r| r.ty == "tool_end" && r.payload["name"] == "edit")
        .map(|r| r.payload)
        .collect();
    assert_eq!(logged.len(), 2, "{logged:#?}");
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
    // Events and the journal carry the code and the manifest.
    let events: Vec<Value> =
        String::from_utf8_lossy(&out.stdout).lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
    let end = events.iter().find(|e| e["event"] == "tool_end" && e["name"] == "write").expect("tool_end");
    assert_eq!((end["ok"].clone(), end["code"].clone()), (json!(false), json!("unknown_tool")));
    let shell_end = events.iter().find(|e| e["event"] == "tool_end" && e["name"] == "shell").unwrap();
    assert_eq!((shell_end["code"].clone(), shell_end["kind"].clone()), (json!("exit"), json!("tail")));
    let manifest = events.iter().find(|e| e["event"] == "manifest").expect("manifest event");
    assert!(!manifest["tools"].to_string().contains("\"write\""), "{manifest}");
    assert_eq!(manifest["editor"], "replace");
    let head = env.journal().into_iter().next().unwrap();
    assert_eq!(head.payload["manifest"]["tools"], manifest["tools"]);
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
    let session = |n: usize| env.sessions()[n].clone();

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
fn mcp_tools_join_the_catalog_when_called_and_obey_disabled() {
    // Nothing routes to calc, so the model's call is what starts it. The
    // server's listed tools then exist like any other, minus the disabled one;
    // a name it never listed does not exist at all.
    let env = Env::new(
        "mcp-catalog",
        json!({ "steps": [
            { "call": "mcp__calc__add", "args": { "a": 2, "b": 3 } },
            { "call": "mcp__calc__echo", "args": { "text": "hi" } },
            { "call": "mcp__calc__probe_env", "args": {} },
            { "text": "done" }
        ] }),
    );
    env.config(&format!(
        "[router]\nmode = \"local\"\n[tools]\ndisabled = [\"mcp__calc__echo\"]\n[mcp.calc]\ncommand = \"{}\"\nargs = [\"mcp-fake\"]\ndescription = \"calculator\"\n",
        dev_bin().display()
    ));
    let out = env.run(&["run", "--no-learn", "say hello"]);
    assert!(out.status.success(), "{}", text(&out));
    let reqs = env.requests();
    assert!(!tool_names(&reqs[0]).iter().any(|n| n.starts_with("mcp__")), "nothing routed");
    assert_eq!(tool_names(&reqs[1]).iter().filter(|n| n.starts_with("mcp__")).collect::<Vec<_>>(), ["mcp__calc__add"]);
    assert_eq!(
        tool_results(&env),
        ["5", "Error: unknown tool mcp__calc__echo", "Error: unknown tool mcp__calc__probe_env"]
    );
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

fn has(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

#[test]
fn tui_wheel_scrolls_without_mouse_capture_and_copy_writes_osc52() {
    let long: String = (1..=80).map(|i| format!("line-{i:02}\n\n")).collect();
    let env = Env::new(
        "tui-scroll",
        json!({ "steps": [
            { "text": long },
            { "call": "read", "args": { "path": "missing.txt" } },
            { "text": "final-reply" }
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
    let (short, long_t) = (budget(3000), budget(5000));
    let run = |d: &mut drive::Driver, log: &mut Vec<u8>, script: String| {
        if let Err(e) = drive::run_script(d, &script, log) {
            panic!("{e:#}\n{}", String::from_utf8_lossy(log));
        }
    };
    run(
        &mut d,
        &mut log,
        format!("wait \"ask construct\" {short}\ntype go\nkey enter\nwait \"line-80\" {long_t}\nexpect \"line-80\"\n"),
    );
    assert!(has(&d.raw, b"\x1b[?1007h"), "alternate scroll is on");
    assert!(!has(&d.raw, b"\x1b[?1000h"), "mouse capture stays off");
    // The wheel in alternate-scroll mode is just up/down keys.
    d.send(&b"\x1b[A".repeat(10)).unwrap();
    run(&mut d, &mut log, format!("wait \"line-5\" {short}\ngone \"line-80\" {short}\n"));
    d.send(&b"\x1b[B".repeat(40)).unwrap();
    run(&mut d, &mut log, format!("wait \"line-80\" {short}\n"));
    // /copy: the last assistant message, then the last tool error, as OSC 52.
    run(
        &mut d,
        &mut log,
        format!(
            "type another\nkey enter\nwait \"final-reply\" {long_t}\ntype /copy\nkey enter\nwait \"copied last reply\" {short}\n"
        ),
    );
    let b64 = |s: &str| {
        let out = std::process::Command::new("base64")
            .arg("-w0")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        use std::io::Write;
        out.stdin.as_ref().unwrap().write_all(s.as_bytes()).unwrap();
        String::from_utf8(out.wait_with_output().unwrap().stdout).unwrap()
    };
    assert!(has(&d.raw, format!("\x1b]52;c;{}\x07", b64("final-reply")).as_bytes()), "/copy wrote the reply");
    run(&mut d, &mut log, format!("type /copy error\nkey enter\nwait \"copied last tool error\" {short}\n"));
    assert!(has(&d.raw, b"\x1b]52;c;"), "/copy error wrote a sequence");
    d.send(b"/quit\r").unwrap();
    std::thread::sleep(Duration::from_millis(500));
    d.pump(Duration::from_millis(budget(500)));
    assert!(has(&d.raw, b"\x1b[?1007l"), "alternate scroll reset on exit");
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
    let id = env.sessions()[0].clone();
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
    let tools = |r: &Value| -> Vec<String> {
        r["tools"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|t| t["function"]["name"].as_str().map(String::from))
            .collect()
    };
    // The prompt says to load project skills with the skill tool, so it's there,
    // whatever the router made of "hi".
    assert!(
        tools(env.requests().last().unwrap()).iter().any(|t| t == "skill"),
        "skill tool missing in a repo with skills"
    );
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
    assert!(!tools(env.requests().last().unwrap()).iter().any(|t| t == "skill"), "no project skills, no skill tool");
    // Outside any repo there is no project to write to.
    let out = in_dir(&env.work, &["run", "--no-learn", "note this"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(last_tool_result(env.requests().last().unwrap()).contains("not in a git repo"));
}

// ------------------------------------------------------------------ journal

/// A row's payload as the one `Event` type, back to JSON: equal values mean
/// equal events, and a payload that isn't an `Event` fails here.
fn as_event(v: &Value) -> Value {
    let ev: construct::event::Event = serde_json::from_value(v.clone()).unwrap_or_else(|e| panic!("{e}: {v}"));
    serde_json::to_value(ev).unwrap()
}

fn json_events(out: &std::process::Output) -> Vec<Value> {
    String::from_utf8_lossy(&out.stdout).lines().map(|l| serde_json::from_str(l).unwrap()).collect()
}

#[test]
fn journal_holds_a_turn_as_the_same_events_json_prints() {
    let env = Env::new(
        "journal",
        json!({ "steps": [ { "call": "read", "args": { "path": "a.txt" } }, { "text": "It says hello." } ] }),
    );
    env.config("[router]\nmode = \"local\"\n");
    std::fs::write(env.work.join("a.txt"), "hello\n").unwrap();
    let out = env.run(&["run", "--json", "--no-learn", "what does a.txt say?"]);
    assert!(out.status.success(), "{}", text(&out));
    let rows = env.journal();
    let id = env.sessions().pop().expect("a session");
    assert!(rows.iter().all(|r| r.session == id && r.v == 1 && r.at_ms > 0));
    assert!(rows.windows(2).all(|w| w[0].seq < w[1].seq));
    let types: Vec<&str> = rows.iter().map(|r| r.ty.as_str()).collect();
    assert_eq!(types[..2], ["head", "system"], "{types:?}");
    assert_eq!(rows[0].payload["cwd"], env.work.display().to_string());
    // The request rows carry what the snapshots pin, for the bodies actually sent.
    let reqs = env.requests();
    let sent: Vec<&Row> = rows.iter().filter(|r| r.ty == "request").collect();
    assert_eq!(sent.len(), reqs.len());
    for (row, req) in sent.iter().zip(&reqs) {
        assert_eq!(row.payload["bytes"].as_u64().unwrap() as usize, serde_json::to_vec(req).unwrap().len());
        assert_eq!(row.payload["n_tools"].as_u64().unwrap() as usize, tool_names(req).len());
        assert_eq!(row.payload["tools"], json!(tool_names(req)));
    }
    // The message rows are what the model saw, verbatim.
    let messages: Vec<Value> =
        rows.iter().filter(|r| r.payload.get("message").is_some()).map(|r| r.payload["message"].clone()).collect();
    let roles: Vec<&str> = messages.iter().map(|m| m["role"].as_str().unwrap()).collect();
    assert_eq!(roles, ["system", "user", "assistant", "tool", "assistant"]);
    let seen = reqs[1]["messages"].as_array().unwrap();
    assert_eq!(seen.len(), 4);
    for (m, s) in messages.iter().zip(seen).skip(1) {
        assert_eq!(
            (&m["role"], &m["tool_calls"], &m["tool_call_id"]),
            (&s["role"], &s["tool_calls"], &s["tool_call_id"])
        );
        // The wire sends an empty assistant content as null; the row omits it.
        assert_eq!(m.get("content").unwrap_or(&Value::Null), &s["content"]);
    }
    // The tool result is one indexed lookup by call id.
    let call_id = messages[2]["tool_calls"][0]["id"].as_str().unwrap().to_string();
    let db = rusqlite::Connection::open(env.journal_path(&env.work)).unwrap();
    let by_id: Vec<String> = db
        .prepare("SELECT type FROM events WHERE json_extract(payload, '$.id') = ?1 ORDER BY seq")
        .unwrap()
        .query_map([&call_id], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(by_id, ["tool_start", "tool_end", "tool_result"]);
    let plan: String = db
        .query_row("EXPLAIN QUERY PLAN SELECT seq FROM events WHERE json_extract(payload, '$.id') = 'x'", [], |r| {
            r.get(3)
        })
        .unwrap();
    assert!(plan.contains("events_id"), "{plan}");
    // --json printed the same events, plus the stream deltas the journal skips.
    let printed: Vec<Value> = json_events(&out)
        .iter()
        .filter(|e| !["opened", "text", "reasoning", "tool_pending"].contains(&e["event"].as_str().unwrap()))
        .map(as_event)
        .collect();
    let stored: Vec<Value> = rows.iter().map(|r| as_event(&r.payload)).collect();
    assert_eq!(printed, stored);
}

fn usage_events(out: &std::process::Output) -> Vec<Value> {
    json_events(out).into_iter().filter(|e| e["event"] == "usage").collect()
}

fn read_then_answer() -> Value {
    json!({ "steps": [ { "call": "read", "args": { "path": "a.txt" } }, { "text": "It says hello." } ] })
}

#[test]
fn plain_endpoints_get_no_breakpoints_and_cache_hits_are_journaled() {
    let env = Env::new("cache-plain", read_then_answer());
    env.config("[router]\nmode = \"local\"\n");
    std::fs::write(env.work.join("a.txt"), "hello\n").unwrap();
    let out = env.run(&["run", "--json", "--no-learn", "what does a.txt say?"]);
    assert!(out.status.success(), "{}", text(&out));
    for req in env.requests() {
        assert!(req.get("cache_control").is_none() && req.get("usage").is_none(), "{req}");
        assert!(req["messages"].as_array().unwrap().iter().all(|m| m["content"].is_string() || m["content"].is_null()));
    }
    // The mock caches prefixes on its own: the second call reuses the first.
    let usage = usage_events(&out);
    assert_eq!(usage.len(), 2);
    assert_eq!(usage[0]["cached"], 0);
    let (cached, prompt) = (usage[1]["cached"].as_u64().unwrap(), usage[1]["prompt"].as_u64().unwrap());
    assert!(cached > 0 && cached < prompt, "{usage:?}");
    assert_eq!(usage[1]["cache_write"], 0);
    assert!(usage[1].get("cost").is_none());
    let end = json_events(&out).into_iter().find(|e| e["event"] == "turn_end").unwrap();
    assert_eq!(end["cached"], cached);
    let stored: Vec<Value> = env.journal().into_iter().filter(|r| r.ty == "usage").map(|r| r.payload).collect();
    assert_eq!(stored, usage);
}

#[test]
fn openrouter_anthropic_requests_carry_a_breakpoint_unless_cache_is_off() {
    let env = Env::new("cache-openrouter", read_then_answer());
    let cfg =
        "model = \"anthropic/claude-haiku-5.5\"\n[router]\nmode = \"local\"\n[provider]\ndialect = \"openrouter\"\n";
    env.config(cfg);
    std::fs::write(env.work.join("a.txt"), "hello\n").unwrap();
    let out = env.run(&["run", "--json", "--no-learn", "what does a.txt say?"]);
    assert!(out.status.success(), "{}", text(&out));
    let reqs = env.requests();
    assert_eq!(reqs.len(), 2);
    for req in &reqs {
        assert_eq!(req["cache_control"], json!({ "type": "ephemeral" }));
        assert!(req.get("usage").is_none(), "OpenRouter reports cost unasked: {req}");
    }
    let usage = usage_events(&out);
    assert!(usage.iter().all(|u| u["cache_write"].as_u64().unwrap() > 0), "{usage:?}");
    assert!(usage[1]["cached"].as_u64().unwrap() > 0, "{usage:?}");
    let head = json_events(&out).into_iter().find(|e| e["event"] == "head").unwrap();
    assert_eq!((&head["manifest"]["dialect"], &head["manifest"]["cache"]), (&json!("openrouter"), &json!("auto")));

    env.config(&format!("{cfg}cache = \"off\"\n"));
    let out = env.run(&["run", "--json", "--no-learn", "and now?"]);
    assert!(out.status.success(), "{}", text(&out));
    let reqs = env.requests();
    assert!(reqs.len() > 2);
    assert!(reqs[2..].iter().all(|r| r.get("cache_control").is_none()));
}

#[test]
fn continue_resumes_from_the_journal() {
    let env = Env::new(
        "journal-continue",
        json!({ "steps": [ { "call": "read", "args": { "path": "a.txt" } }, { "text": "It says hello." }, { "text": "Elsewhere." }, { "text": "Still hello." } ] }),
    );
    env.config("[router]\nmode = \"local\"\n");
    std::fs::write(env.work.join("a.txt"), "hello\n").unwrap();
    let out = env.run(&["run", "--no-learn", "what does a.txt say?"]);
    assert!(out.status.success(), "{}", text(&out));
    // A session elsewhere is not "the latest session in this directory".
    let other = env.dir.join("other");
    std::fs::create_dir_all(&other).unwrap();
    let out = env.run_in(&other, &[], &["run", "--no-learn", "unrelated"]);
    assert!(out.status.success(), "{}", text(&out));
    // What a resume from the old JSONL would have loaded: every logged
    // message. The export is that log.
    let exported = env.run(&["sessions", "export", &env.sessions()[0]]);
    assert!(exported.status.success(), "{}", text(&exported));
    let logged: Vec<Value> = String::from_utf8_lossy(&exported.stdout)
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .filter(|v| v["type"] == "message")
        .map(|v| v["message"].clone())
        .collect();
    let out = env.run(&["run", "--no-learn", "-c", "and now?"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(text(&out).contains("Still hello."));
    assert_eq!(env.sessions().len(), 1, "continued, not a new session");
    let reqs = env.requests();
    let sent = reqs.last().unwrap()["messages"].as_array().unwrap();
    let shape =
        |m: &Value| (m["role"].clone(), m["content"].clone(), m["tool_calls"].clone(), m["tool_call_id"].clone());
    // The system prompt is rebuilt on resume; the rest is replayed exactly,
    // then the new turn.
    assert_eq!(sent.len(), logged.len() + 1);
    assert_eq!(
        sent[1..logged.len()].iter().map(shape).collect::<Vec<_>>(),
        logged[1..].iter().map(shape).collect::<Vec<_>>()
    );
    assert_eq!(sent.last().unwrap()["content"], "and now?");
    assert_eq!(sent[4]["content"], "It says hello.");
}

#[test]
fn concurrent_sessions_share_a_project_journal_and_another_project_has_its_own() {
    let mut steps = vec![];
    for _ in 0..4 {
        steps.push(json!({ "call": "read", "args": { "path": "a.txt" } }));
    }
    let env = Env::new("journal-concurrent", json!({ "steps": steps }));
    env.config("[router]\nmode = \"local\"\n");
    // Two clones of one repo (same remote) and an unrelated repo.
    let repo = |name: &str, remote: &str| {
        let d = env.dir.join(name);
        std::fs::create_dir_all(d.join(".git")).unwrap();
        std::fs::write(d.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::write(d.join(".git/config"), format!("[remote \"origin\"]\n\turl = {remote}\n")).unwrap();
        std::fs::write(d.join("a.txt"), "hello\n").unwrap();
        d
    };
    let a = repo("clone-a", "git@github.com:me/proj.git");
    let b = repo("clone-b", "https://github.com/me/proj");
    let other = repo("other", "git@github.com:me/other.git");
    assert_eq!(env.journal_path(&a), env.journal_path(&b), "clones share one journal");
    assert_ne!(env.journal_path(&a), env.journal_path(&other));
    let spawn = |dir: &PathBuf| {
        let mut c = Command::new(construct());
        c.args(["run", "--no-learn", "read a.txt"]).current_dir(dir);
        for (k, v) in env.env() {
            if v.is_empty() {
                c.env_remove(k);
            } else {
                c.env(k, v);
            }
        }
        c.stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).spawn().unwrap()
    };
    let children: Vec<_> = [&a, &b, &a, &b].into_iter().map(spawn).collect();
    for c in children {
        let out = c.wait_with_output().unwrap();
        assert!(out.status.success(), "{}", text(&out));
    }
    let out = env.run_in(&other, &[], &["run", "--no-learn", "read a.txt"]);
    assert!(out.status.success(), "{}", text(&out));
    let shared = env.journal_in(&a);
    let heads: Vec<&Row> = shared.iter().filter(|r| r.ty == "head").collect();
    assert_eq!(heads.len(), 4);
    // Every session is whole: a head, then its turn through to the end.
    for h in &heads {
        let mine: Vec<&str> = shared.iter().filter(|r| r.session == h.session).map(|r| r.ty.as_str()).collect();
        assert_eq!((mine[0], *mine.last().unwrap()), ("head", "turn_end"), "{mine:?}");
        assert!(mine.contains(&"assistant"));
    }
    let own = env.journal_in(&other);
    assert_eq!(own.iter().filter(|r| r.ty == "head").count(), 1);
    assert!(own.iter().all(|r| heads.iter().all(|h| h.session != r.session)));
}

#[test]
fn sessions_export_writes_the_old_jsonl_shape() {
    let env = Env::new(
        "journal-export",
        json!({ "steps": [
            { "call": "edit", "args": { "path": "a.txt", "old_string": "nope", "new_string": "x" } },
            { "text": "Could not edit." }
        ] }),
    );
    env.config("[router]\nmode = \"local\"\n");
    std::fs::write(env.work.join("a.txt"), "hello\n").unwrap();
    let out = env.run(&["run", "--no-learn", "edit it"]);
    assert!(out.status.success(), "{}", text(&out));
    let id = env.sessions()[0].clone();
    let out = env.run(&["sessions", "export", &id]);
    assert!(out.status.success(), "{}", text(&out));
    let lines: Vec<Value> = json_events(&out);
    let types: Vec<&str> = lines.iter().map(|l| l["type"].as_str().unwrap()).collect();
    assert_eq!(
        types,
        ["session", "message", "route", "message", "usage", "message", "tool", "message", "usage", "message"]
    );
    let head = &lines[0];
    assert_eq!((head["id"].as_str(), head["harness"].as_str()), (Some(id.as_str()), Some("construct")));
    assert_eq!(head["cwd"], env.work.display().to_string());
    assert!(head["manifest"]["tools"].is_array() && head["at"].is_u64());
    // Messages round-trip verbatim; the tool record is the bench's.
    let rows = env.journal();
    let stored: Vec<&Value> = rows.iter().filter_map(|r| r.payload.get("message")).collect();
    let exported: Vec<&Value> = lines.iter().filter(|l| l["type"] == "message").map(|l| &l["message"]).collect();
    assert_eq!(stored, exported);
    let tool = &lines[6]["data"];
    assert_eq!((tool["name"].as_str(), tool["ok"].as_bool()), (Some("edit"), Some(false)));
    assert_eq!(tool["id"], lines[5]["message"]["tool_calls"][0]["id"]);
    assert!(tool["code"].is_string() && tool.get("ms").is_some() && tool.get("data").is_some(), "{tool}");
    assert!(lines[4]["usage"]["prompt_tokens"].is_u64() && lines[4]["model"].is_string());
    assert!(lines[2]["data"]["groups"].is_array());
    let missing = env.run(&["sessions", "export", "no-such-session"]);
    assert!(!missing.status.success());
}

#[test]
fn sandbox_keeps_the_journal_out_of_reach() {
    let name = "journal-sandbox";
    let dir = Env::dir_for(name);
    let journal = construct::journal::path(&construct::config::Paths { home: dir.join("home") }, &dir.join("work"));
    let j = journal.display().to_string();
    let env = Env::new(
        name,
        json!({ "steps": [
            { "call": "shell", "args": { "command": format!("head -c 16 '{j}' && echo JOURNAL-READ; ls '{}' && echo DIR-LISTED", journal.parent().unwrap().display()) } },
            { "call": "shell", "args": { "command": format!("sqlite3 '{j}' 'select count(*) from events' && echo SQL-READ") } },
            { "call": "read", "args": { "path": j } },
            { "call": "grep", "args": { "pattern": "event", "path": journal.parent().unwrap().display().to_string() } },
            { "text": "done" }
        ] }),
    );
    let out = env.run(&["run", "--no-learn", "look at the history"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(journal.exists(), "the session was journaled");
    let r = tool_results(&env);
    assert_eq!(r.len(), 4, "{r:#?}");
    assert!(
        !r[0].contains("SQLite format") && !r[0].contains("JOURNAL-READ") && !r[0].contains("DIR-LISTED"),
        "{}",
        r[0]
    );
    assert!(!r[1].contains("SQL-READ"), "{}", r[1]);
    assert!(r[2].contains("blocked by the sandbox"), "{}", r[2]);
    assert!(r[3].contains("blocked by the sandbox"), "{}", r[3]);
}

// ------------------------------------------------------------------ history

/// One `journal_query` per SQL, then "done"; returns each call's result.
fn journal_queries(name: &str, sqls: &[&str]) -> (Env, Vec<String>) {
    let mut steps: Vec<Value> = sqls.iter().map(|s| json!({ "call": "journal_query", "args": { "sql": s } })).collect();
    steps.push(json!({ "text": "done" }));
    let env = Env::new(name, json!({ "steps": steps }));
    env.config("[router]\nmode = \"local\"\n");
    let out = env.run(&["run", "--no-learn", "what did we do in the last session?"]);
    assert!(out.status.success(), "{}", text(&out));
    let r = tool_results(&env);
    assert_eq!(r.len(), sqls.len(), "{r:#?}");
    (env, r)
}

#[test]
fn recall_result_returns_an_earlier_turns_tool_output() {
    let env = Env::new(
        "history-recall",
        json!({ "steps": [
            { "call": "read", "args": { "path": "a.txt" } },
            { "text": "It says hello." },
            { "call": "recall_result", "args": { "id": "call_1" } },
            { "text": "Recalled." }
        ] }),
    );
    env.config("[router]\nmode = \"local\"\n");
    std::fs::write(env.work.join("a.txt"), "hello from the first turn\n").unwrap();
    let out = env.run(&["run", "--no-learn", "what does a.txt say?"]);
    assert!(out.status.success(), "{}", text(&out));
    let first = env.requests()[1].clone();
    assert_eq!(first["messages"].as_array().unwrap().last().unwrap()["tool_call_id"], "call_1");
    let original = last_tool_result(&first);
    assert!(original.contains("hello from the first turn"), "{original}");
    // The file changes; the recall must come from the journal, in a new session.
    std::fs::write(env.work.join("a.txt"), "changed\n").unwrap();
    let out = env.run(&["run", "--no-learn", "show me what you read last time"]);
    assert!(out.status.success(), "{}", text(&out));
    let reqs = env.requests();
    assert!(tool_names(&reqs[2]).contains(&"recall_result".to_string()), "history routed");
    assert_eq!(last_tool_result(&reqs[3]), original);
    assert_eq!(env.sessions().len(), 2);
}

#[test]
fn journal_query_returns_rows() {
    let (_, r) = journal_queries("history-query", &["SELECT type, count(*) FROM events GROUP BY type ORDER BY type"]);
    assert!(r[0].starts_with("type | count(*)"), "{}", r[0]);
    for row in ["head | 1", "user | 1", "tool_start | 1"] {
        assert!(r[0].lines().any(|l| l == row), "{row} in {}", r[0]);
    }
}

#[test]
fn journal_query_refuses_anything_but_one_select() {
    let (env, r) = journal_queries(
        "history-refuse",
        &[
            "DELETE FROM events",
            "PRAGMA journal_mode=DELETE",
            "ATTACH DATABASE 'other.sqlite' AS other",
            "WITH x AS (SELECT 1) DELETE FROM events",
            "SELECT 1; DELETE FROM events",
            "SELECT * FROM pragma_journal_mode",
        ],
    );
    for (i, out) in r.iter().enumerate() {
        assert!(out.contains("journal_query runs one read-only SELECT"), "query {i}: {out}");
    }
    assert!(!env.work.join("other.sqlite").exists());
    let db = rusqlite::Connection::open(env.journal_path(&env.work)).unwrap();
    let mode: String = db.query_row("PRAGMA journal_mode", [], |r| r.get(0)).unwrap();
    assert_eq!(mode, "wal");
    let n: i64 = db.query_row("SELECT count(*) FROM events WHERE type = 'head'", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 1, "nothing deleted");
}

#[test]
fn journal_query_stops_a_slow_query() {
    let t = std::time::Instant::now();
    let (_, r) = journal_queries(
        "history-timeout",
        &["WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c) SELECT count(*) FROM c"],
    );
    assert!(r[0].contains("query stopped after 5 s"), "{}", r[0]);
    assert!(t.elapsed() < Duration::from_millis(budget(15_000)), "{:?}", t.elapsed());
}

#[test]
fn journal_query_cuts_an_oversized_result_with_a_note() {
    let (_, r) = journal_queries(
        "history-cap",
        &[
            "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c LIMIT 100) SELECT x, replace(hex(zeroblob(500)), '0', 'a') FROM c",
            "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c LIMIT 1000) SELECT x FROM c",
        ],
    );
    assert!(r[0].contains("[result cut at 16 KB"), "{}", &r[0][r[0].len().saturating_sub(300)..]);
    assert!(r[0].len() < 17 * 1024, "{}", r[0].len());
    assert!(r[1].contains("[result cut at 200 rows"), "{}", r[1]);
    assert!(r[1].lines().any(|l| l == "200") && !r[1].lines().any(|l| l == "201"), "{}", r[1]);
}

#[test]
fn a_history_turn_works_without_chatstrata_on_path() {
    let env = Env::new(
        "history-no-chatstrata",
        json!({ "steps": [
            { "call": "journal_query", "args": { "sql": "SELECT e.type FROM events_fts JOIN events e ON e.seq = events_fts.rowid WHERE events_fts MATCH 'parser'" } },
            { "text": "We talked about the parser." }
        ] }),
    );
    env.config("[router]\nmode = \"local\"\n");
    let empty = env.dir.join("empty-path");
    std::fs::create_dir_all(&empty).unwrap();
    let out = env.run_with(
        &[("PATH", empty.to_str().unwrap())],
        &["run", "--no-learn", "what did we decide about the parser last time?"],
    );
    assert!(out.status.success(), "{}", text(&out));
    assert!(text(&out).contains("We talked about the parser."));
    let r = tool_results(&env);
    assert!(r[0].starts_with("type\nuser"), "{}", r[0]);
    assert!(!tool_names(&env.requests()[0]).iter().any(|n| n.contains("session_search")));
}

// ------------------------------------------------------------- mcp sandbox

fn fake_server(name: &str, extra: &str) -> String {
    format!(
        "[mcp.{name}]\ncommand = \"{}\"\nargs = [\"mcp-fake\", \"--probes\"]\ndescription = \"probe\"\n{extra}",
        dev_bin().display()
    )
}

#[test]
fn mcp_servers_run_inside_the_sandbox_with_only_their_named_secrets() {
    let name = "mcp-sandbox";
    let dir = Env::dir_for(name);
    let outside = dir.join("outside");
    let allowed = dir.join("allowed");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let env = Env::new(
        name,
        json!({ "steps": [
            { "call": "mcp__boxed__probe_read", "args": { "path": outside.join("x.txt") } },
            { "call": "mcp__boxed__probe_read", "args": { "path": "in-work.txt" } },
            { "call": "mcp__boxed__probe_env", "args": {} },
            { "call": "mcp__boxed__probe_connect", "args": { "addr": addr } },
            { "call": "mcp__opened__probe_read", "args": { "path": allowed.join("x.txt") } },
            { "call": "mcp__opened__probe_connect", "args": { "addr": addr } },
            { "text": "done" }
        ] }),
    );
    for d in [&outside, &allowed] {
        std::fs::create_dir_all(d).unwrap();
        std::fs::write(d.join("x.txt"), "outside the workspace").unwrap();
    }
    std::fs::write(env.work.join("in-work.txt"), "inside").unwrap();
    env.config(&format!(
        "[router]\nmode = \"local\"\n{}{}",
        fake_server("boxed", "network = false\nsecrets = [\"FAKE_SERVICE_TOKEN\", \"UNSET_SERVICE_TOKEN\"]\n"),
        fake_server("opened", &format!("allow_read = [\"{}\"]\n", allowed.display())),
    ));
    let out = env.run_with(
        &[("FAKE_SERVICE_TOKEN", "named-secret"), ("OTHER_SERVICE_TOKEN", "unnamed-secret")],
        &["run", "--no-learn", "probe boxed and opened"],
    );
    assert!(out.status.success(), "{}", text(&out));
    let r = tool_results(&env);
    assert_eq!(r.len(), 6, "{r:#?}");
    assert!(r[0].starts_with("DENIED"), "outside the workspace: {}", r[0]);
    assert_eq!(r[1], "READ inside");
    assert!(r[2].lines().any(|l| l == "FAKE_SERVICE_TOKEN=named-secret"), "{}", r[2]);
    for leaked in ["OTHER_SERVICE_TOKEN", "OPENAI_API_KEY", "UNSET_SERVICE_TOKEN"] {
        assert!(!r[2].contains(leaked), "{leaked} reached the server");
    }
    assert!(r[3].starts_with("DENIED"), "network = false: {}", r[3]);
    assert_eq!(r[4], "READ outside the workspace", "allow_read");
    assert_eq!(r[5], "CONNECTED", "network on by default");
}

#[test]
fn nothing_an_mcp_server_starts_outlives_construct() {
    let env = Env::new(
        "mcp-lifetime",
        json!({ "steps": [ { "call": "mcp__boxed__probe_spawn", "args": {} }, { "text": "done" } ] }),
    );
    env.config(&format!("[router]\nmode = \"local\"\n{}", fake_server("boxed", "")));
    let out = env.run(&["run", "--no-learn", "probe boxed"]);
    assert!(out.status.success(), "{}", text(&out));
    let pid = tool_results(&env)[0].clone();
    assert!(pid.parse::<u32>().is_ok(), "{pid}");
    let alive = || std::fs::read(format!("/proc/{pid}/cmdline")).is_ok_and(|c| c == b"sleep\x00300\x00");
    let t = std::time::Instant::now();
    while alive() && t.elapsed() < Duration::from_millis(budget(3000)) {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(!alive(), "setsid sleep started by the server is still running");
}

/// A real server over the network: Context7's docs MCP through npx. Needs
/// node and the internet, so it runs only with `--ignored`.
#[test]
#[ignore]
fn context7_answers_from_inside_the_sandbox() {
    let env = Env::new(
        "mcp-context7",
        json!({ "steps": [
            { "call": "mcp__context7__resolve-library-id", "args": { "libraryName": "rusqlite", "query": "interrupt a long query" } },
            { "text": "done" }
        ] }),
    );
    env.config(
        "[router]\nmode = \"local\"\n[mcp.context7]\ncommand = \"npx\"\nargs = [\"-y\", \"@upstash/context7-mcp\"]\ndescription = \"library documentation\"\nsecrets = [\"CONTEXT7_API_KEY\"]\n",
    );
    let out = env.run(&["run", "--no-learn", "use context7 for the rusqlite docs"]);
    assert!(out.status.success(), "{}", text(&out));
    let r = tool_results(&env);
    assert!(r[0].contains("/rusqlite/rusqlite"), "{}", r[0]);
}

// ---------------------------------------------------------------- mcp spec

fn spec_server(name: &str, flags: &str, extra: &str) -> String {
    format!(
        "[mcp.{name}]\ncommand = \"{}\"\nargs = [\"mcp-fake\"{flags}]\ndescription = \"spec cases\"\n{extra}",
        dev_bin().display()
    )
}

#[test]
fn mcp_client_answers_server_requests_skips_bad_lines_and_keeps_rich_content() {
    use construct::mcp::wire_name;
    let env = Env::new(
        "mcp-spec",
        json!({ "steps": [
            { "call": "mcp__spec__ping_first", "args": {} },
            { "call": "mcp__spec__ask_sampling", "args": {} },
            { "call": "mcp__spec__garbage", "args": {} },
            { "call": "mcp__spec__echo", "args": { "text": "after garbage" } },
            { "call": "mcp__spec__rich", "args": {} },
            { "call": "mcp__spec__structured", "args": {} },
            { "call": wire_name("spec", "dotted.tool"), "args": {} },
            { "call": wire_name("spec", construct_dev::mcp_fake::LONG_NAME), "args": {} },
            { "text": "done" }
        ] }),
    );
    env.config(&format!("[router]\nmode = \"local\"\n{}", spec_server("spec", ", \"--spec\"", "")));
    let out = env.run(&["run", "--no-learn", "try the spec server"]);
    assert!(out.status.success(), "{}", text(&out));
    let r = tool_results(&env);
    assert_eq!(
        r,
        [
            "pong ok",
            "refused ok",
            "survived",
            "after garbage",
            "intro\nembedded notes\n[link parser.rs file:///src/parser.rs]",
            r#"{"answer":42}"#,
            "dotted ok",
            "long ok"
        ]
    );
    let first = &env.requests()[0];
    for name in tool_names(first) {
        assert!(
            name.len() <= 64 && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'),
            "{name} is not a valid function name"
        );
    }
    assert!(!first["tools"].to_string().contains("$schema"), "dialect key forwarded");
}

#[test]
fn a_crashed_mcp_server_fails_the_call_at_once_and_restarts_on_the_next() {
    let env = Env::new(
        "mcp-crash",
        json!({ "steps": [
            { "call": "mcp__spec__crash", "args": {} },
            { "call": "mcp__spec__echo", "args": { "text": "back" } },
            { "text": "done" }
        ] }),
    );
    env.config(&format!("[router]\nmode = \"local\"\n{}", spec_server("spec", ", \"--spec\"", "")));
    let t = std::time::Instant::now();
    let out = env.run(&["run", "--no-learn", "try the spec server"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(t.elapsed() < Duration::from_millis(budget(10_000)), "waited on a dead server: {:?}", t.elapsed());
    let r = tool_results(&env);
    assert!(r[0].contains("MCP server closed"), "{}", r[0]);
    assert_eq!(r[1], "back");
}

#[test]
fn mcp_progress_keeps_a_call_alive_and_silence_cancels_it() {
    let env = Env::new(
        "mcp-progress",
        json!({ "steps": [
            { "call": "mcp__spec__slow", "args": { "ms": 2500 } },
            { "call": "mcp__spec__hang", "args": {} },
            { "text": "done" }
        ] }),
    );
    env.config(&format!("[router]\nmode = \"local\"\n{}", spec_server("spec", ", \"--spec\"", "timeout = 1\n")));
    let out = env.run(&["run", "--no-learn", "try the spec server"]);
    assert!(out.status.success(), "{}", text(&out));
    let r = tool_results(&env);
    assert_eq!(r[0], "slow done", "progress every 200ms outlasts a 1s timeout");
    assert!(r[1].contains("timed out after 1s"), "{}", r[1]);
    let log = std::fs::read_to_string(env.work.join("mcp-fake.log")).unwrap_or_default();
    assert!(log.lines().any(|l| l.starts_with("cancelled ")), "no notifications/cancelled: {log:?}");
}

#[test]
fn mcp_startup_problems_are_reported_with_the_reason() {
    let env = Env::new("mcp-startup", json!({ "steps": [ { "text": "done" } ] }));
    env.config(&format!(
        "[router]\nmode = \"local\"\n{}{}{}",
        spec_server("broken", ", \"--fail-start\"", ""),
        spec_server("future", ", \"--version\", \"2999-01-01\"", ""),
        spec_server("bare", ", \"--no-tools\"", ""),
    ));
    // The turn runs; the failed starts are error events (and a failing exit).
    let out = env.run(&["run", "--no-learn", "--json", "use broken, future and bare"]);
    let errors: Vec<String> = json_events(&out)
        .iter()
        .filter(|e| e["event"] == "error")
        .map(|e| e["message"].as_str().unwrap_or("").to_string())
        .collect();
    assert_eq!(errors.len(), 2, "{errors:#?}");
    assert!(errors.iter().any(|e| e.contains("MCP broken") && e.contains("cannot open config")), "{errors:#?}");
    assert!(
        errors
            .iter()
            .any(|e| e.contains("MCP future") && e.contains("unsupported MCP protocol version \"2999-01-01\"")),
        "{errors:#?}"
    );
    assert!(!tool_names(&env.requests()[0]).iter().any(|n| n.starts_with("mcp__")), "no tools from any of them");
}

#[test]
fn an_mcp_server_name_that_cannot_round_trip_is_refused() {
    let env = Env::new("mcp-name", json!({ "steps": [ { "text": "done" } ] }));
    env.config(&spec_server("my__server", "", ""));
    let out = env.run(&["run", "--no-learn", "hi"]);
    assert!(!out.status.success());
    assert!(text(&out).contains("[mcp.my__server]"), "{}", text(&out));
}

#[test]
fn skill_tools_reject_misrouted_args_and_say_where_a_skill_lives() {
    let skill_md = "---\nname: where-am-i\ndescription: Finding where skills live\n---\n\n1. look\n";
    let env = Env::new(
        "skill-paths",
        json!({ "steps": [
            { "call": "skill_manage", "args": { "action": "create", "name": "where-am-i", "content": skill_md } },
            { "call": "skill", "args": { "action": "edit", "name": "where-am-i", "new_string": "x" } },
            { "call": "skill", "args": {} },
            { "call": "skill", "args": { "name": "where-am-i" } },
            { "text": "done" }
        ] }),
    );
    let out = env.run(&["run", "--no-learn", "make a skill"]);
    assert!(out.status.success(), "{}", text(&out));
    let r = tool_results(&env);
    let path = env.home.join("skills/where-am-i/SKILL.md");
    let dir = env.home.join("skills/where-am-i").display().to_string();
    assert!(r[0].contains("created skill where-am-i") && r[0].contains("global: ") && r[0].contains(&dir), "{}", r[0]);
    assert!(r[1].contains("skill_manage") && r[1].contains("action"), "{}", r[1]);
    assert!(r[2].contains("skill_manage"), "{}", r[2]);
    assert!(r[3].starts_with("[global: ") && r[3].contains(&dir), "{}", r[3]);
    assert!(std::fs::read_to_string(path).unwrap().contains("1. look"), "edit must not have changed anything");
}

#[test]
fn tui_status_shows_the_session_and_totals_that_the_journal_holds() {
    let env = Env::new("tui-status", json!({ "steps": [ { "text": "first-reply" }, { "text": "second-reply" } ] }));
    let mut d = drive::Driver::spawn(
        &[construct().display().to_string()],
        &env.env(),
        Some(env.work.to_str().unwrap()),
        120,
        30,
    )
    .unwrap();
    let mut log = Vec::new();
    let (short, long) = (budget(3000), budget(5000));
    let script = format!(
        "wait \"ask construct\" {short}\ntype one\nkey enter\nwait \"first-reply\" {long}\n\
         type two\nkey enter\nwait \"second-reply\" {long}\ntype /status\nkey enter\nwait \"requests\" {short}\n"
    );
    if let Err(e) = drive::run_script(&mut d, &script, &mut log) {
        panic!("{e:#}\n{}", String::from_utf8_lossy(&log));
    }
    let screen = d.text();
    let rows = env.journal();
    let head = rows.iter().find(|r| r.ty == "head").expect("head row");
    assert!(screen.contains(&format!("session   {}", head.session)), "session id on screen\n{screen}");
    assert!(screen.contains("journal   ") && screen.contains("journal.sqlite"), "{screen}");
    let usage: Vec<_> = rows.iter().filter(|r| r.ty == "usage").collect();
    assert_eq!(usage.len(), 2);
    let sum = |k: &str| usage.iter().map(|r| r.payload[k].as_u64().unwrap_or(0)).sum::<u64>();
    // Totals on screen are the journal's own sums, formatted the same way.
    let human = construct::journal::human;
    let row = |label: &str| {
        screen.lines().find(|l| l.contains(label)).unwrap_or_else(|| panic!("{label} row\n{screen}")).to_string()
    };
    assert!(screen.contains("╭─ status"), "drawn as a box\n{screen}");
    assert!(row("requests").contains("requests  2"), "{}", row("requests"));
    assert!(
        row("input").contains(&format!("input     {}", human(sum("prompt")))),
        "{} vs prompt {}",
        row("input"),
        sum("prompt")
    );
    assert!(row("output").contains(&format!("output    {}", human(sum("completion")))), "{}", row("output"));
    // The status line carries the same cumulative counters and the id.
    let last = screen.lines().rfind(|l| !l.trim().is_empty()).unwrap().to_string();
    assert!(last.contains(&format!("{} in", human(sum("prompt")))), "{last}");
    let tail = head.session.split_once('-').unwrap().1;
    assert!(last.contains(tail), "{last}");
    // /copy status writes the block as OSC 52.
    if let Err(e) =
        drive::run_script(&mut d, &format!("type /copy status\nkey enter\nwait \"copied status\" {short}\n"), &mut log)
    {
        panic!("{e:#}\n{}", String::from_utf8_lossy(&log));
    }
    assert!(has(&d.raw, b"\x1b]52;c;"), "/copy status wrote a sequence");
    d.kill();
}

#[test]
fn sessions_show_prints_a_session_from_another_project() {
    let env = Env::new(
        "sessions-show",
        json!({ "steps": [ { "call": "read", "args": { "path": "a.txt" } }, { "call": "read", "args": { "path": "nope.txt" } }, { "text": "It says hello." } ] }),
    );
    env.config("[router]\nmode = \"local\"\n");
    let other = env.dir.join("other-project");
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(other.join("a.txt"), "hello\n").unwrap();
    let out = env.run_in(&other, &[], &["run", "--no-learn", "what does a.txt say?"]);
    assert!(out.status.success(), "{}", text(&out));
    let id = env.journal_in(&other).into_iter().find(|r| r.ty == "head").unwrap().session;
    // Asked from a different project: found by id anywhere under the home.
    let shown = env.run(&["sessions", "show", &id]);
    let s = text(&shown);
    assert!(shown.status.success(), "{s}");
    for want in [
        &id,
        "## user",
        "what does a.txt say?",
        "-> read",
        "<- read ok",
        "<- read error",
        "## assistant",
        "It says hello.",
        "usage in",
        "# totals: 3 requests",
    ] {
        assert!(s.contains(want), "missing {want:?} in\n{s}");
    }
    let missing = env.run(&["sessions", "show", "no-such-session"]);
    assert!(!missing.status.success());
}

#[test]
fn home_is_private_and_an_open_one_is_tightened_once() {
    use std::os::unix::fs::PermissionsExt;
    let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    let env =
        Env::new("home-private", json!({ "steps": [ { "text": "one" }, { "text": "two" }, { "text": "three" } ] }));
    // A fresh home is created 0700; everything under it, journals included, is behind it.
    let out = env.run(&["run", "--no-learn", "hi"]);
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(mode(&env.home), 0o700);
    assert!(env.home.join("projects").is_dir());
    assert!(!text(&out).contains("made"), "a new home needs no notice: {}", text(&out));
    // A home from before this change (world-readable, no marker) is tightened once, with a notice.
    std::fs::remove_file(env.home.join(construct::home::MARKER)).unwrap();
    std::fs::set_permissions(&env.home, std::fs::Permissions::from_mode(0o755)).unwrap();
    let out = env.run(&["run", "--no-learn", "hi"]);
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(mode(&env.home), 0o700);
    assert!(text(&out).contains("private (755 -> 700)"), "{}", text(&out));
    // A user who shares it again on purpose is left alone.
    std::fs::set_permissions(&env.home, std::fs::Permissions::from_mode(0o750)).unwrap();
    let out = env.run(&["run", "--no-learn", "hi"]);
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(mode(&env.home), 0o750);
}
