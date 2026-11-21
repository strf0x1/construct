//! Permission prompts end to end: the real binary in a pty with a scripted
//! model, reaching for paths the sandbox keeps it from. What is asserted is
//! what the person sees and presses, what the model gets back, and what the
//! journal and `config.toml` end up holding.

use crate::common::*;
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
/// Tool results in the agent's last request. A learner may call the model
/// after it (no tools: a side call), so that one doesn't count.
fn results(env: &Env) -> Vec<String> {
    let last = env.requests().into_iter().rev().find(|r| r["tools"].as_array().is_some_and(|t| !t.is_empty())).unwrap();
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
    assert_eq!((rows[0]["tool"].as_str(), rows[0]["decision"].as_str()), (Some("bash"), Some("once")));
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
    assert_eq!((rows[0]["tool"].as_str(), rows[0]["decision"].as_str()), (Some("bash"), Some("once")));
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
        .filter(|r| r.ty == "tool_end" && r.payload["name"] == "bash")
        .filter_map(|r| r.payload["data"].get("sandbox").cloned())
        .collect();
    assert_eq!(boxed.len(), 4, "{boxed:?}");
    assert_eq!(boxed.iter().map(|b| b["stop"] == true).collect::<Vec<_>>(), [false, false, true, true]);
    assert_eq!(boxed[3]["refused"], true);
}
