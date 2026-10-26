//! Permission prompts end to end: the real binary in a pty with a scripted
//! model, reaching for paths the sandbox keeps it from. What is asserted is
//! what the person sees and presses, what the model gets back, and what the
//! journal and `config.toml` end up holding.

mod common;

use common::*;
use construct_dev::drive;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// The TUI on a pty in the env's work dir.
fn tui(env: &Env, extra: &[(&str, &str)]) -> drive::Driver {
    let mut vars = env.env();
    vars.extend(extra.iter().map(|(k, v)| (k.to_string(), v.to_string())));
    drive::Driver::spawn(&[construct().display().to_string()], &vars, Some(env.work.to_str().unwrap()), 110, 34)
        .unwrap()
}

/// Run a drive script; on failure, panic with the log (which has the screens).
fn play(d: &mut drive::Driver, script: &str) {
    let mut log = Vec::new();
    if let Err(e) = drive::run_script(d, script, &mut log) {
        panic!("{e:#}\n{}", String::from_utf8_lossy(&log));
    }
}

fn quit(d: &mut drive::Driver) {
    d.send(b"/quit\r").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(300));
}

/// `cat` of a path the command only works out when it runs, so nothing
/// before it can see the path: only the denial in its output names it.
fn hidden_cat(p: &Path) -> String {
    let octal: String = p.display().to_string().bytes().map(|b| format!("\\{b:03o}")).collect();
    format!("cat \"$(printf '{octal}')\"")
}

/// Where a test's file outside the work dir will be (known before its `Env`
/// exists, because the scripted model has to name it).
fn outside_path(test: &str, name: &str) -> PathBuf {
    Env::dir_for(test).join("outside").join(name)
}

fn make(p: &Path, content: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, content).unwrap();
}

fn read_call(p: &Path) -> Value {
    json!({ "call": "read", "args": { "path": p.display().to_string() } })
}

/// What the model was told about each tool call, in order.
fn results(env: &Env) -> Vec<String> {
    let last = env.requests().pop().unwrap();
    last["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["role"] == "tool")
        .map(|m| m["content"].as_str().unwrap_or("").to_string())
        .collect()
}

fn access_rows(env: &Env) -> Vec<Value> {
    env.journal().into_iter().filter(|r| r.ty == "access").map(|r| r.payload).collect()
}

#[test]
fn once_lets_one_call_through_session_lets_the_rest_through() {
    let file = outside_path("perm-once-session", "a.txt");
    let env = Env::new(
        "perm-once-session",
        json!({ "steps": [read_call(&file), read_call(&file), read_call(&file), { "text": "all done" }] }),
    );
    make(&file, "alpha\n");
    let mut d = tui(&env, &[]);
    play(
        &mut d,
        &format!(
            r#"
wait "ask construct" {}
type read it
key enter
wait "PERMISSION" {}
expect "main agent"
expect "Allow once"
expect "Always allow"
expect "a.txt"
sleep 600
type y
wait "allowed once" {}
wait "PERMISSION" {}
sleep 600
type s
wait "allowed for this session" {}
wait "all done" {}
gone "PERMISSION" 500
"#,
            budget(3000),
            budget(5000),
            budget(3000),
            budget(5000),
            budget(3000),
            budget(5000)
        ),
    );
    quit(&mut d);
    // Once did not carry over (the second read asked again); the session
    // grant covered the third without a question.
    let got = results(&env);
    assert!(got.iter().all(|r| r.contains("alpha")), "{got:?}");
    let rows = access_rows(&env);
    let decisions: Vec<&str> = rows.iter().map(|r| r["decision"].as_str().unwrap()).collect();
    assert_eq!(decisions, ["once", "session"], "{rows:?}");
    assert_eq!(rows[0]["tool"], "read");
    assert_eq!(rows[0]["access"], "read");
    // Nothing was written to config.
    assert!(!env.home.join("config.toml").exists());
}

/// What `tui/prompt.rs` promises to look like: choices colored by
/// consequence, and the selection a filled bar that moves with the keys. Text
/// alone can't see any of it.
#[test]
fn the_prompt_colors_choices_by_consequence_and_fills_the_selected_row() {
    let file = outside_path("perm-styled", "c.txt");
    let env = Env::new("perm-styled", json!({ "steps": [read_call(&file), { "text": "all done" }] }));
    make(&file, "charlie\n");
    let image = env.dir.join("prompt.svg");
    let mut d = tui(&env, &[]);
    play(
        &mut d,
        &format!(
            r#"
wait "ask construct" {}
type read it
key enter
wait "PERMISSION" {}
sleep 600
expect-style "PERMISSION" fg=#1a1b26 bold
expect-style "Allow once" fg=#9ece6a bg=#2c3048 bold
expect-style "Always allow" fg=#e0af68 bg=default
expect-style "Deny" fg=#f7768e
key down
sleep 100
expect-style "Allow for this session" bg=#2c3048 bold
expect-style "Allow once" bg=default !bold
image {}
key enter
wait "all done" {}
"#,
            budget(3000),
            budget(5000),
            image.display(),
            budget(5000)
        ),
    );
    quit(&mut d);
    assert_eq!(access_rows(&env)[0]["decision"], "session");
    let svg = std::fs::read_to_string(&image).unwrap();
    assert!(svg.starts_with("<svg") && svg.contains("fill=\"#2c3048\""), "{svg}");
}

#[test]
fn deny_tells_the_model_and_always_writes_the_path_to_config() {
    let file = outside_path("perm-deny-always", "b.txt");
    let env =
        Env::new("perm-deny-always", json!({ "steps": [read_call(&file), read_call(&file), { "text": "all done" }] }));
    make(&file, "bravo\n");
    env.config("# my settings\nmodel = \"test-model\"\n");
    let mut d = tui(&env, &[]);
    play(
        &mut d,
        &format!(
            r#"
wait "ask construct" {}
type read it
key enter
wait "PERMISSION" {}
sleep 600
type n
wait "denied" {}
wait "PERMISSION" {}
sleep 600
type a
wait "all done" {}
"#,
            budget(3000),
            budget(5000),
            budget(3000),
            budget(5000),
            budget(5000)
        ),
    );
    quit(&mut d);
    let got = results(&env);
    assert!(got[0].contains("blocked by the sandbox") && got[0].contains("declined"), "{}", got[0]);
    assert!(got[1].contains("bravo"), "{}", got[1]);
    let rows = access_rows(&env);
    let decisions: Vec<&str> = rows.iter().map(|r| r["decision"].as_str().unwrap()).collect();
    assert_eq!(decisions, ["deny", "always"], "{rows:?}");

    // The file is the user's: their comment and key are still there, and the
    // entry is exactly the path.
    let cfg = std::fs::read_to_string(env.home.join("config.toml")).unwrap();
    assert!(cfg.starts_with("# my settings\nmodel = \"test-model\""), "{cfg}");
    let want = file.canonicalize().unwrap().display().to_string();
    assert!(cfg.contains("[sandbox]") && cfg.contains(&format!("allow_read = [\"{want}\"]")), "{cfg}");

    // A later session starts with it: no question, and headless works too.
    let env2 = Env::new("perm-deny-always-2", json!({ "steps": [read_call(&file), { "text": "fine" }] }));
    std::fs::create_dir_all(&env2.home).unwrap();
    std::fs::copy(env.home.join("config.toml"), env2.home.join("config.toml")).unwrap();
    let out = env2.run(&["run", "--json", "--no-learn", "read it"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(results(&env2)[0].contains("bravo"));
}

#[test]
fn a_headless_run_still_denies_and_records_no_decision_nobody_made() {
    let file = outside_path("perm-headless", "c.txt");
    let env = Env::new("perm-headless", json!({ "steps": [read_call(&file), { "text": "ok" }] }));
    make(&file, "charlie\n");
    let out = env.run(&["run", "--json", "--no-learn", "read it"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(results(&env)[0].starts_with("Error: blocked by the sandbox"), "{}", results(&env)[0]);
    assert!(access_rows(&env).is_empty());
}

#[test]
fn a_shell_command_the_sandbox_stopped_is_offered_again_with_the_path() {
    let file = outside_path("perm-shell", "d.txt");
    // Built at run time, so it can't be seen before the command runs: only
    // the denial in its output names it.
    let cmd = hidden_cat(&file);
    let env = Env::new(
        "perm-shell",
        json!({ "steps": [{ "call": "shell", "args": { "command": cmd } }, { "text": "all done" }] }),
    );
    make(&file, "delta\n");
    let mut d = tui(&env, &[]);
    play(
        &mut d,
        &format!(
            r#"
wait "ask construct" {}
type cat it
key enter
wait "PERMISSION" {}
expect "A shell command was blocked"
expect "runs the command again"
expect "d.txt"
sleep 600
type y
wait "all done" {}
"#,
            budget(3000),
            budget(8000),
            budget(5000)
        ),
    );
    quit(&mut d);
    let got = results(&env);
    assert!(got[0].contains("delta") && !got[0].contains("exit"), "the rerun succeeded: {}", got[0]);
    let rows = access_rows(&env);
    assert_eq!((rows[0]["tool"].as_str(), rows[0]["decision"].as_str()), (Some("shell"), Some("once")));
    assert_eq!(rows[0]["paths"][0].as_str(), Some(file.canonicalize().unwrap().to_str().unwrap()));
}

#[test]
fn ssh_and_gnupg_are_never_offered_and_the_model_is_told_why() {
    let home = Env::dir_for("perm-protected").join("fakehome");
    let key = home.join(".ssh/id_ed25519");
    let env = Env::new("perm-protected", json!({ "steps": [read_call(&key), { "text": "all done" }] }));
    make(&key, "PRIVATE\n");
    let mut d = tui(&env, &[("HOME", home.to_str().unwrap())]);
    play(
        &mut d,
        &format!(
            r#"
wait "ask construct" {}
type read it
key enter
wait "all done" {}
gone "PERMISSION" 300
"#,
            budget(3000),
            budget(5000)
        ),
    );
    quit(&mut d);
    let got = results(&env);
    assert!(got[0].contains("protected") && !got[0].contains("PRIVATE"), "{}", got[0]);
    assert!(access_rows(&env).is_empty());
}

#[test]
fn esc_dismisses_the_question_with_the_turn() {
    let file = outside_path("perm-esc", "e.txt");
    let env = Env::new("perm-esc", json!({ "steps": [read_call(&file), { "text": "back again" }] }));
    make(&file, "echo\n");
    let mut d = tui(&env, &[]);
    play(
        &mut d,
        &format!(
            r#"
wait "ask construct" {}
type read it
key enter
wait "PERMISSION" {}
key esc
gone "PERMISSION" {}
wait "interrupted" {}
"#,
            budget(3000),
            budget(5000),
            budget(3000),
            budget(3000)
        ),
    );
    quit(&mut d);
}

// Delegated agents ---------------------------------------------------------

/// A TUI session whose model delegates `times` tasks to the scripted foreign
/// ACP agent `ext`.
fn ext_tui(name: &str, times: usize, extra_cfg: &str) -> (Env, drive::Driver) {
    let delegate = || json!({ "call": "delegate", "args": { "agent": "ext", "task": "clean the build dir" } });
    let mut steps: Vec<Value> = (0..times).map(|_| delegate()).collect();
    steps.push(json!({ "text": "the agent finished" }));
    // One more turn for a test that runs headless afterwards.
    steps.extend([delegate(), json!({ "text": "the agent finished again" })]);
    let env = Env::new(name, json!({ "steps": steps }));
    let bin = dev_bin();
    let dir = bin.parent().unwrap().display();
    env.config(&format!(
        "[sandbox]\nallow_read = [\"{dir}\"]\n[agents.ext]\ndescription = \"a foreign agent\"\nprotocol = \"acp\"\nallow_read = [\"{dir}\"]\ncommand = [\"{}\", \"acp-fake\"]\n{extra_cfg}\n",
        bin.display(),
    ));
    let d = tui(&env, &[]);
    (env, d)
}

fn ask_script(key: &str, verdict_word: &str) -> String {
    format!(
        r#"
wait "ask construct" {}
type ask ext
key enter
wait "PERMISSION" {}
expect "subagent · ext"
expect "rm -rf build"
expect "(execute)"
sleep 600
type {key}
wait "{verdict_word}" {}
wait "the agent finished" {}
"#,
        budget(3000),
        budget(8000),
        budget(3000),
        budget(8000)
    )
}

fn permission_rows(env: &Env) -> Vec<Value> {
    env.journal().into_iter().filter(|r| r.ty == "permission").map(|r| r.payload).collect()
}

#[test]
fn a_foreign_agents_request_is_put_to_the_user_and_allow_reaches_it() {
    let (env, mut d) = ext_tui("perm-ext-allow", 1, "");
    play(&mut d, &ask_script("y", "allowed once"));
    quit(&mut d);
    assert!(results(&env)[0].starts_with("done: opt-allow_once"), "{}", results(&env)[0]);
    let rows = permission_rows(&env);
    assert_eq!((rows[0]["decision"].as_str(), rows[0]["scope"].as_str()), (Some("allow"), Some("once")));
}

#[test]
fn deny_gives_the_agent_a_reject_option_and_never_an_allow() {
    let (env, mut d) = ext_tui("perm-ext-deny", 1, "");
    play(&mut d, &ask_script("n", "denied"));
    quit(&mut d);
    assert!(results(&env)[0].starts_with("done: opt-reject_once"), "{}", results(&env)[0]);
    assert_eq!(permission_rows(&env)[0]["decision"], "reject");
}

#[test]
fn session_remembers_a_kind_for_that_agent_and_asks_nothing_more() {
    let (env, mut d) = ext_tui("perm-ext-session", 2, "");
    play(
        &mut d,
        &format!(
            r#"
wait "ask construct" {}
type ask ext twice
key enter
wait "PERMISSION" {}
sleep 600
type s
wait "allowed for this session" {}
wait "the agent finished" {}
gone "PERMISSION" 300
"#,
            budget(3000),
            budget(8000),
            budget(3000),
            budget(10000)
        ),
    );
    quit(&mut d);
    let got = results(&env);
    assert!(got[0].starts_with("done: opt-allow_once") && got[1].starts_with("done: opt-allow_once"), "{got:?}");
    let scopes: Vec<String> = permission_rows(&env).iter().map(|r| r["scope"].as_str().unwrap().to_string()).collect();
    assert_eq!(scopes, ["session", "session"]);
    assert!(std::fs::read_to_string(env.home.join("config.toml")).unwrap().find("allow_kinds").is_none());
}

#[test]
fn always_saves_the_kind_to_the_agents_definition_and_later_sessions_do_not_ask() {
    let (env, mut d) = ext_tui("perm-ext-always", 1, "");
    play(&mut d, &ask_script("a", "always allowed"));
    quit(&mut d);
    let cfg = std::fs::read_to_string(env.home.join("config.toml")).unwrap();
    assert!(cfg.contains("allow_kinds = [\"execute\"]"), "{cfg}");
    // Headless, with that config, the same request is allowed by config.
    let out = env.run(&["run", "--json", "--no-learn", "ask ext"]);
    assert!(out.status.success(), "{}", text(&out));
    let rows = permission_rows(&env);
    let last = rows.last().unwrap();
    assert_eq!((last["decision"].as_str(), last["scope"].as_str()), (Some("allow"), Some("config")));
}

// A construct child ---------------------------------------------------------

/// A TUI session that delegates once to `scout` (a construct child that can
/// only read), which reads `file`.
fn scout_tui(name: &str, file: &Path, scout_cfg: &str) -> (Env, drive::Driver) {
    let env = Env::new(
        name,
        json!({ "steps": [
            { "call": "delegate", "args": { "agent": "scout", "task": "read the file" } },
            read_call(file),
            { "text": "it holds: contents" },
            { "text": "the scout reported" }
        ] }),
    );
    make(file, "scout-data\n");
    env.config(&format!(
        "[agents.scout]\ndescription = \"reads files and reports\"\nprompt = \"You are a scout.\"\ntools = [\"read\"]\n{scout_cfg}\n"
    ));
    let d = tui(&env, &[]);
    (env, d)
}

#[test]
fn a_construct_childs_blocked_read_asks_the_user_and_once_lets_that_call_through() {
    let file = outside_path("perm-child-once", "f.txt");
    let (env, mut d) = scout_tui("perm-child-once", &file, "");
    play(
        &mut d,
        &format!(
            r#"
wait "ask construct" {}
type ask scout
key enter
wait "PERMISSION" {}
expect "subagent · scout"
expect "read is blocked by the sandbox"
gone "Always allow" 300
sleep 600
type y
wait "the scout reported" {}
"#,
            budget(3000),
            budget(10000),
            budget(10000)
        ),
    );
    quit(&mut d);
    let back = results(&env);
    assert!(back[0].starts_with("it holds: contents"), "{back:?}");
    // The child's decision is in its own session's rows, written by the parent.
    let sessions = env.sessions();
    let child_access: Vec<Value> =
        env.journal().into_iter().filter(|r| r.ty == "access" && r.session == sessions[1]).map(|r| r.payload).collect();
    assert_eq!(child_access.len(), 1, "{child_access:?}");
    assert_eq!(child_access[0]["decision"], "once");
    // And the read itself succeeded: the summary is scripted, this is not.
    let got = tool_results(&env, &sessions[1]);
    assert!(got.iter().any(|r| r.contains("scout-data")), "{got:?}");
}

#[test]
fn always_for_a_child_goes_in_its_definition_when_the_parent_could_reach_the_path() {
    // The path is inside the parent's work dir, which the child was not given.
    let file = Env::dir_for("perm-child-always").join("work/data/f.txt");
    let (env, mut d) = scout_tui("perm-child-always", &file, "");
    play(
        &mut d,
        &format!(
            r#"
wait "ask construct" {}
type ask scout
key enter
wait "PERMISSION" {}
expect "Always allow"
expect "[agents.scout]"
sleep 600
type a
wait "the scout reported" {}
"#,
            budget(3000),
            budget(10000),
            budget(10000)
        ),
    );
    quit(&mut d);
    let cfg = std::fs::read_to_string(env.home.join("config.toml")).unwrap();
    let want = file.canonicalize().unwrap().display().to_string();
    assert!(cfg.contains("[agents.scout]") && cfg.contains(&format!("allow_read = [\"{want}\"]")), "{cfg}");
    assert!(results(&env)[0].starts_with("it holds: contents"));
}

// Headless ------------------------------------------------------------------

#[test]
fn headless_an_agent_asks_nobody_and_allow_kinds_still_applies() {
    // `ask` with nobody to ask is a reject, as the default always was.
    let env = Env::new(
        "perm-ext-headless-ask",
        json!({ "steps": [
            { "call": "delegate", "args": { "agent": "ext", "task": "clean" } },
            { "text": "ok" }
        ] }),
    );
    let bin = dev_bin();
    let dir = bin.parent().unwrap().display();
    let cfg = |extra: &str| {
        format!(
            "[sandbox]\nallow_read = [\"{dir}\"]\n[agents.ext]\nprotocol = \"acp\"\nallow_read = [\"{dir}\"]\ncommand = [\"{}\", \"acp-fake\"]\n{extra}\n",
            bin.display()
        )
    };
    env.config(&cfg("on_permission = \"ask\""));
    let out = env.run(&["run", "--json", "--no-learn", "go"]);
    assert!(out.status.success(), "{}", text(&out));
    let rows = permission_rows(&env);
    assert_eq!((rows[0]["decision"].as_str(), rows[0]["scope"].as_str()), (Some("reject"), Some("deny")));
    assert!(access_rows(&env).is_empty());

    // A kind on the allow list is answered without asking, even with reject.
    let env = Env::new(
        "perm-ext-headless-kinds",
        json!({ "steps": [
            { "call": "delegate", "args": { "agent": "ext", "task": "clean" } },
            { "text": "ok" }
        ] }),
    );
    env.config(&cfg("on_permission = \"reject\"\nallow_kinds = [\"execute\"]"));
    let out = env.run(&["run", "--json", "--no-learn", "go"]);
    assert!(out.status.success(), "{}", text(&out));
    let rows = permission_rows(&env);
    assert_eq!((rows[0]["decision"].as_str(), rows[0]["scope"].as_str()), (Some("allow"), Some("config")));
}

#[test]
fn tab_switches_the_scope_to_the_folder_so_its_siblings_pass() {
    let (a, b) = (outside_path("perm-folder", "a.txt"), outside_path("perm-folder", "b.txt"));
    let env = Env::new("perm-folder", json!({ "steps": [read_call(&a), read_call(&b), { "text": "all done" }] }));
    make(&a, "first\n");
    make(&b, "second\n");
    let mut d = tui(&env, &[]);
    play(
        &mut d,
        &format!(
            r#"
wait "ask construct" {}
type read both
key enter
wait "PERMISSION" {}
expect "this path"
expect "its whole folder"
sleep 600
key tab
sleep 100
type s
wait "all done" {}
gone "PERMISSION" 300
"#,
            budget(3000),
            budget(5000),
            budget(5000)
        ),
    );
    quit(&mut d);
    let got = results(&env);
    assert!(got[0].contains("first") && got[1].contains("second"), "{got:?}");
    let rows = access_rows(&env);
    assert_eq!(rows.len(), 1, "the sibling needed no question: {rows:?}");
    assert_eq!(rows[0]["paths"][0].as_str(), Some(a.parent().unwrap().canonicalize().unwrap().to_str().unwrap()));
}

/// What a session's `read` calls returned, from its journal rows.
fn tool_results(env: &Env, session: &str) -> Vec<String> {
    env.journal()
        .into_iter()
        .filter(|r| r.ty == "tool_end" && r.session == session)
        .map(|r| r.payload.to_string())
        .collect()
}

#[test]
fn approving_a_childs_directory_read_gives_it_the_listing() {
    let dir = outside_path("perm-child-dir", "listing");
    let (env, mut d) = scout_tui("perm-child-dir", &dir, "");
    std::fs::remove_file(&dir).unwrap(); // scout_tui made a file there
    make(&dir.join("alpha.txt"), "a\n");
    make(&dir.join("beta.txt"), "b\n");
    play(
        &mut d,
        &format!(
            r#"
wait "ask construct" {}
type ask scout
key enter
wait "PERMISSION" {}
sleep 600
type y
wait "the scout reported" {}
"#,
            budget(3000),
            budget(10000),
            budget(10000)
        ),
    );
    quit(&mut d);
    let sessions = env.sessions();
    let got = tool_results(&env, &sessions[1]);
    assert!(got.iter().any(|r| r.contains("alpha.txt") && r.contains("beta.txt")), "{got:?}");
}

#[test]
fn approving_the_sessions_own_directory_read_once_gives_the_listing() {
    let dir = outside_path("perm-dir-once", "listing");
    let env = Env::new("perm-dir-once", json!({ "steps": [read_call(&dir), { "text": "all done" }] }));
    make(&dir.join("alpha.txt"), "a\n");
    let mut d = tui(&env, &[]);
    play(
        &mut d,
        &format!(
            r#"
wait "ask construct" {}
type read it
key enter
wait "PERMISSION" {}
sleep 600
type y
wait "all done" {}
"#,
            budget(3000),
            budget(10000),
            budget(10000)
        ),
    );
    quit(&mut d);
    assert!(results(&env)[0].contains("alpha.txt"), "{:?}", results(&env));
}

/// A TUI session that delegates once to `scout`, which makes `call`.
fn child_tui(name: &str, call: Value, scout_cfg: &str, extra: &[(&str, &str)]) -> (Env, drive::Driver) {
    let env = Env::new(
        name,
        json!({ "steps": [
            { "call": "delegate", "args": { "agent": "scout", "task": "do it" } },
            call,
            { "text": "child done" },
            { "text": "the scout reported" }
        ] }),
    );
    env.config(&format!("[agents.scout]\ndescription = \"does things\"\nprompt = \"You are a scout.\"\n{scout_cfg}\n"));
    let d = tui(&env, extra);
    (env, d)
}

fn ask_scout_script() -> String {
    format!("wait \"ask construct\" {}\ntype ask scout\nkey enter\n", budget(3000))
}

#[test]
fn a_childs_shell_command_runs_again_after_approval_and_succeeds() {
    let file = outside_path("perm-child-shell", "d.txt");
    // Named only at run time, so the child learns of it from the denial.
    let cmd = hidden_cat(&file);
    let (env, mut d) = child_tui(
        "perm-child-shell",
        json!({ "call": "shell", "args": { "command": cmd } }),
        "tools = [\"shell\"]",
        &[],
    );
    make(&file, "delta\n");
    play(
        &mut d,
        &format!(
            "{}wait \"PERMISSION\" {}\nexpect \"runs the command again\"\nsleep 600\ntype y\nwait \"the scout reported\" {}\n",
            ask_scout_script(),
            budget(10000),
            budget(10000)
        ),
    );
    quit(&mut d);
    let got = tool_results(&env, &env.sessions()[1]);
    assert!(got.iter().any(|r| r.contains("delta")), "{got:?}");
}

#[test]
fn a_childs_shell_command_naming_an_outside_path_asks_the_parent_before_it_runs() {
    let file = outside_path("perm-child-ahead", "d.txt");
    let cmd = format!("cat {} 2>/dev/null || echo missing", file.display());
    let (env, mut d) = child_tui(
        "perm-child-ahead",
        json!({ "call": "shell", "args": { "command": cmd } }),
        "tools = [\"shell\"]",
        &[],
    );
    make(&file, "delta\n");
    play(
        &mut d,
        &format!(
            "{}wait \"PERMISSION\" {}\nexpect \"subagent\"\nexpect \"wants to read paths outside the sandbox\"\nsleep 600\ntype y\nwait \"the scout reported\" {}\n",
            ask_scout_script(),
            budget(10000),
            budget(10000)
        ),
    );
    quit(&mut d);
    let got = tool_results(&env, &env.sessions()[1]);
    assert!(got.iter().any(|r| r.contains("delta") && !r.contains("missing")), "{got:?}");
}

#[test]
fn a_protected_path_is_refused_to_a_child_without_a_prompt() {
    let home = Env::dir_for("perm-child-protected").join("fakehome");
    let key = home.join(".ssh/id_ed25519");
    let (env, mut d) =
        child_tui("perm-child-protected", read_call(&key), "tools = [\"read\"]", &[("HOME", home.to_str().unwrap())]);
    make(&key, "PRIVATE\n");
    play(
        &mut d,
        &format!("{}wait \"the scout reported\" {}\ngone \"PERMISSION\" 300\n", ask_scout_script(), budget(10000)),
    );
    quit(&mut d);
    let got = tool_results(&env, &env.sessions()[1]);
    assert!(got.iter().any(|r| r.contains("protected")) && !got.iter().any(|r| r.contains("PRIVATE")), "{got:?}");
    assert!(access_rows(&env).is_empty());
}

#[test]
fn a_childs_shell_still_cannot_open_tcp_beyond_its_endpoint() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let cmd = format!("timeout 5 bash -c 'exec 3<>/dev/tcp/127.0.0.1/{port}'; echo rc=$?");
    let (env, mut d) =
        child_tui("perm-child-net", json!({ "call": "shell", "args": { "command": cmd } }), "tools = [\"shell\"]", &[]);
    play(&mut d, &format!("{}wait \"the scout reported\" {}\n", ask_scout_script(), budget(10000)));
    quit(&mut d);
    let got = tool_results(&env, &env.sessions()[1]);
    assert!(got.iter().any(|r| r.contains("rc=1")), "{got:?}");
    assert!(listener.set_nonblocking(true).is_ok() && listener.accept().is_err(), "the child connected");
}

#[test]
fn a_shell_command_naming_an_outside_path_asks_before_it_runs() {
    let dir = outside_path("perm-shell-ahead", "sibling");
    let cmd = format!("ls {} 2>/dev/null || echo \"Directory does not exist\"", dir.display());
    let env = Env::new(
        "perm-shell-ahead",
        json!({ "steps": [{ "call": "shell", "args": { "command": cmd } }, { "text": "all done" }] }),
    );
    make(&dir.join("other.txt"), "more\n");
    let mut d = tui(&env, &[]);
    play(
        &mut d,
        &format!(
            r#"
wait "ask construct" {}
type list the sibling
key enter
wait "PERMISSION" {}
expect "wants to read paths outside the sandbox"
expect "Allow and run the command"
gone "runs the command again" 100
sleep 600
type y
wait "all done" {}
"#,
            budget(3000),
            budget(8000),
            budget(5000)
        ),
    );
    quit(&mut d);
    let got = results(&env);
    assert!(got[0].contains("other.txt") && !got[0].contains("does not exist"), "{}", got[0]);
    assert!(!got[0].contains("[sandbox:"), "{}", got[0]);
    let rows = access_rows(&env);
    assert_eq!((rows[0]["tool"].as_str(), rows[0]["decision"].as_str()), (Some("shell"), Some("once")));
}

/// #64: headless, nobody can be asked, and the commands hide their errors
/// the way models write them. The model is still told what the sandbox kept
/// from it, and from the third blocked call in a turn to stop and ask.
#[test]
fn hidden_sandbox_denials_still_reach_the_model_and_a_hunt_is_stopped() {
    let dir = outside_path("perm-shell-hidden", "ptest");
    let shell = |cmd: String| json!({ "call": "shell", "args": { "command": cmd } });
    let env = Env::new("perm-shell-hidden", json!({ "steps": [] }));
    let skill = env.home.join("skills/demo/SKILL.md");
    let steps = json!({ "steps": [
        shell(format!("cat {}", skill.display())),
        shell(format!("ls -la {} 2>/dev/null || echo \"ptest dir does not exist\"", dir.display())),
        shell("find / -maxdepth 1 -name nothing-here 2>/dev/null".into()),
        shell(format!("ls -la {} 2>/dev/null", env.home.display())),
        // Past the stop: named outside, so not run at all (the marker stays unmade).
        shell(format!("touch ran-anyway; ls {} 2>/dev/null", dir.display())),
        // Nothing outside named: runs as ever.
        shell("echo still-works".into()),
        { "text": "all done" }
    ]});
    let env = Env::new("perm-shell-hidden", steps);
    make(&skill, "---\nname: demo\n---\nskill body\n");
    make(&dir.join("notes.txt"), "n\n");
    let out = env.run(&["run", "--json", "--no-learn", "find my notes"]);
    assert!(out.status.success(), "{}", text(&out));
    let got = results(&env);
    // A skill's own files are readable by the shell.
    assert!(got[0].contains("skill body") && !got[0].contains("[sandbox:"), "{}", got[0]);
    // Exit 0, the error thrown away, and a fallback that isn't true.
    assert!(got[1].contains("ptest dir does not exist"), "{}", got[1]);
    assert!(got[1].contains("is outside the sandbox") && got[1].contains("doesn't mean it isn't there"), "{}", got[1]);
    assert!(got[2].contains("only the workspace, system paths and toolchains are visible under /"), "{}", got[2]);
    assert!(!got[2].contains("Stop searching"), "{}", got[2]);
    assert!(got[3].contains("construct's own files") && got[3].contains("skill_manage"), "{}", got[3]);
    assert!(got[3].contains("3 shell calls this turn were blocked") && got[3].contains("ask the user"), "{}", got[3]);
    assert!(got[4].contains("not run") && got[4].contains("ask the user"), "{}", got[4]);
    assert!(!env.work.join("ran-anyway").exists(), "a refused command must not run");
    assert!(got[5].contains("still-works") && !got[5].contains("[sandbox:"), "{}", got[5]);
    assert!(access_rows(&env).is_empty(), "nobody was asked");
    // The same, as data for scorers: one row per blocked call, the stop from the third.
    let boxed: Vec<Value> = env
        .journal()
        .into_iter()
        .filter(|r| r.ty == "tool_end" && r.payload["name"] == "shell")
        .filter_map(|r| r.payload["data"].get("sandbox").cloned())
        .collect();
    assert_eq!(boxed.len(), 4, "{boxed:?}");
    assert_eq!(boxed.iter().map(|b| b["stop"] == true).collect::<Vec<_>>(), [false, false, true, true]);
    assert_eq!(boxed[3]["refused"], true);
}
