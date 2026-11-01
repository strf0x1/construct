//! Developer operations through the real TUI. A local repository and bare
//! remote prove that approval authorizes the captured content, even when the
//! index or branch changes while the user reviews it. No network writes.

use crate::common::*;
use construct_dev::drive;
use serde_json::{Value, json};
use std::path::Path;
use std::process::Command;

fn git(work: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(["-c", "core.hooksPath=/dev/null", "-c", "commit.gpgsign=false"])
        .args(args)
        .current_dir(work)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", text(&out));
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

fn env(name: &str, args: Value) -> Env {
    env_format(name, args, "sha1")
}

fn env_format(name: &str, args: Value, object_format: &str) -> Env {
    let env = Env::new(name, json!({"steps": [{"call":"developer", "args":args}, {"text":"developer done"}]}));
    env.config("[learn]\nenabled = false\n[router]\nmode = \"local\"\n");
    git(&env.work, &["init", "-b", "main", &format!("--object-format={object_format}")]);
    git(&env.work, &["config", "user.name", "Construct Test"]);
    git(&env.work, &["config", "user.email", "construct@example.invalid"]);
    std::fs::write(env.work.join("notes.txt"), "baseline\n").unwrap();
    git(&env.work, &["add", "notes.txt"]);
    git(&env.work, &["commit", "-m", "baseline"]);
    env
}

fn stage(env: &Env, contents: &str) {
    std::fs::write(env.work.join("notes.txt"), contents).unwrap();
    git(&env.work, &["add", "notes.txt"]);
}

fn play(driver: &mut drive::Driver, script: &str) {
    let mut log = Vec::new();
    if let Err(e) = drive::run_script(driver, script, &mut log) {
        panic!("{e:#}\n{}", String::from_utf8_lossy(&log));
    }
}

fn reviewing(env: &Env) -> drive::Driver {
    let mut vars = env.env();
    let auth = env.dir.join("auth");
    if auth.is_dir() {
        vars.push(("HOME".into(), auth.display().to_string()));
        vars.push(("XDG_CONFIG_HOME".into(), auth.join("config").display().to_string()));
    }
    let mut driver =
        drive::Driver::spawn(&[construct().display().to_string()], &vars, Some(env.work.to_str().unwrap()), 110, 34)
            .unwrap();
    play(
        &mut driver,
        &format!(
            "wait \"ask construct\" {}\ntype perform the developer operation\nkey enter\nwait \"PERMISSION\" {}\nwait \"Uses host authentication\" {}\nwait \"Allow this operation\" {}\nwait \"Allow this exact operation this session\" {}\n",
            budget(3000),
            budget(8000),
            budget(8000),
            budget(8000),
            budget(8000)
        ),
    );
    driver
}

fn answer(driver: &mut drive::Driver, key: &str) {
    play(driver, &format!("sleep 600\ntype {key}\nwait \"developer done\" {}\n", budget(8000)));
    driver.send(b"/quit\r").unwrap();
}

fn result(env: &Env, operation: &str, decision: &str, ok: bool) -> Value {
    let rows = env.journal();
    let decisions: Vec<_> = rows.iter().filter(|r| r.ty == "operation").collect();
    assert_eq!(decisions.len(), 1, "{rows:?}");
    assert_eq!(decisions[0].payload["operation"], operation);
    assert_eq!(decisions[0].payload["decision"], decision);
    let results: Vec<_> = rows.iter().filter(|r| r.ty == "tool_end" && r.payload["name"] == "developer").collect();
    assert_eq!(results.len(), 1, "{rows:?}");
    assert_eq!(results[0].payload["ok"], ok, "{:?}", results[0]);
    results[0].payload.clone()
}

#[test]
fn approving_a_commit_records_its_staged_content_and_skips_hooks() {
    let env = env("developer-commit", json!({"operation":"commit", "message":"Approved commit"}));
    stage(&env, "prepared content\n");
    let hook = env.work.join(".git/hooks/pre-commit");
    std::fs::write(&hook, "#!/bin/sh\ntouch hook-ran\nexit 1\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut driver = reviewing(&env);
    answer(&mut driver, "y");
    result(&env, "commit", "once", true);
    assert_eq!(git(&env.work, &["log", "-1", "--format=%s"]), "Approved commit");
    assert_eq!(git(&env.work, &["show", "HEAD:notes.txt"]), "prepared content");
    assert!(!env.work.join("hook-ran").exists());
}

#[test]
fn denying_a_commit_keeps_head_and_the_staged_changes() {
    let env = env("developer-commit-deny", json!({"operation":"commit", "message":"Denied commit"}));
    stage(&env, "unpublished content\n");
    let before = git(&env.work, &["rev-parse", "HEAD"]);
    let staged = git(&env.work, &["write-tree"]);
    let mut driver = reviewing(&env);
    answer(&mut driver, "n");
    let output = result(&env, "commit", "deny", false);
    assert_eq!(output["code"], "denied");
    assert_eq!(git(&env.work, &["rev-parse", "HEAD"]), before);
    assert_eq!(git(&env.work, &["write-tree"]), staged);
}

#[test]
fn changing_the_staged_index_cannot_substitute_the_approved_commit_content() {
    let env = env("developer-commit-substitution", json!({"operation":"commit", "message":"Frozen content"}));
    stage(&env, "content presented for approval\n");
    let before = git(&env.work, &["rev-parse", "HEAD"]);
    let mut driver = reviewing(&env);
    stage(&env, "substituted while waiting for approval\n");
    let changed_tree = git(&env.work, &["write-tree"]);
    answer(&mut driver, "y");
    let output = result(&env, "commit", "once", false);
    assert_eq!(output["code"], "changed");
    assert_eq!(git(&env.work, &["rev-parse", "HEAD"]), before);
    assert_eq!(git(&env.work, &["write-tree"]), changed_tree, "approval must not overwrite a later staging decision");
}

/// Trusted host SSH configuration connects to a local receiver instead of a
/// network. Workspace configuration cannot select this executable.
fn remote(env: &Env) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let remote = env.dir.join("remote.git");
    let object_format = git(&env.work, &["rev-parse", "--show-object-format"]);
    git(&env.work, &["init", "--bare", &format!("--object-format={object_format}"), remote.to_str().unwrap()]);
    git(&env.work, &["remote", "add", "origin", "ssh://fixture.invalid/owner/repo"]);
    let auth = env.dir.join("auth");
    std::fs::create_dir_all(&auth).unwrap();
    let shim = auth.join("ssh-fixture");
    let quoted_remote = format!("'{}'", remote.display().to_string().replace('\'', "'\\''"));
    std::fs::write(&shim, format!(
        "#!/bin/sh\nfor arg in \"$@\"; do case \"$arg\" in -G) exit 1;; esac; done\nunset GIT_DIR GIT_COMMON_DIR GIT_OBJECT_DIRECTORY GIT_ALTERNATE_OBJECT_DIRECTORIES GIT_CONFIG_PARAMETERS GIT_CONFIG_COUNT GIT_WORK_TREE GIT_INDEX_FILE\nexport GIT_CONFIG_GLOBAL=/dev/null\nexport GIT_CONFIG_NOSYSTEM=1\nexec /usr/bin/git -c core.hooksPath=/dev/null receive-pack {quoted_remote}\n"
    )).unwrap();
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
    let config = auth.join(".gitconfig");
    git(&env.work, &["config", "--file", config.to_str().unwrap(), "core.sshCommand", shim.to_str().unwrap()]);
    git(&env.work, &["config", "--file", config.to_str().unwrap(), "ssh.variant", "ssh"]);
    remote
}

#[test]
fn approving_a_push_sends_the_captured_commit_when_the_branch_moves() {
    let env = env("developer-push", json!({"operation":"push", "remote":"origin", "branch":"main"}));
    let remote = remote(&env);
    use std::os::unix::fs::PermissionsExt;
    let receive_hook = remote.join("hooks/pre-receive");
    std::fs::write(&receive_hook, "#!/bin/sh\ntouch target-hook-ran\nexit 1\n").unwrap();
    std::fs::set_permissions(&receive_hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    let approved = git(&env.work, &["rev-parse", "HEAD"]);
    let mut driver = reviewing(&env);
    stage(&env, "new content after the push was prepared\n");
    git(&env.work, &["commit", "-m", "later commit"]);
    let later = git(&env.work, &["rev-parse", "HEAD"]);
    assert_ne!(approved, later);
    answer(&mut driver, "y");
    result(&env, "push", "once", true);
    assert_eq!(git(&remote, &["rev-parse", "refs/heads/main"]), approved);
    assert_eq!(git(&env.work, &["rev-parse", "HEAD"]), later);
    assert_eq!(
        git(&remote, &["show", "refs/heads/main:notes.txt"]),
        "baseline",
        "the destination must receive the objects, not only the ref"
    );
    assert!(!remote.join("target-hook-ran").exists(), "the trusted test transport disables its receiver hooks");
}

#[test]
fn approving_a_sha256_push_transfers_the_commit_and_its_objects() {
    let env =
        env_format("developer-push-sha256", json!({"operation":"push", "remote":"origin", "branch":"main"}), "sha256");
    let remote = remote(&env);
    let approved = git(&env.work, &["rev-parse", "HEAD"]);
    assert_eq!(approved.len(), 64);
    let mut driver = reviewing(&env);
    answer(&mut driver, "y");
    result(&env, "push", "once", true);
    assert_eq!(git(&remote, &["rev-parse", "refs/heads/main"]), approved);
    assert_eq!(git(&remote, &["show", "refs/heads/main:notes.txt"]), "baseline");
}

#[test]
fn denying_a_push_creates_no_remote_ref() {
    let env = env("developer-push-deny", json!({"operation":"push", "remote":"origin", "branch":"main"}));
    let remote = remote(&env);
    let mut driver = reviewing(&env);
    answer(&mut driver, "n");
    result(&env, "push", "deny", false);
    assert_eq!(git(&remote, &["for-each-ref", "--format=%(refname)"]), "");
}

#[test]
fn approving_a_commit_in_a_linked_worktree_updates_only_its_branch() {
    let env = env(
        "developer-worktree",
        json!({"operation":"commit", "workdir":"linked", "message":"Linked worktree commit"}),
    );
    let before = git(&env.work, &["rev-parse", "HEAD"]);
    let linked = env.work.join("linked");
    git(&env.work, &["worktree", "add", "-b", "topic", linked.to_str().unwrap()]);
    std::fs::write(linked.join("notes.txt"), "linked content\n").unwrap();
    git(&linked, &["add", "notes.txt"]);
    let mut driver = reviewing(&env);
    answer(&mut driver, "y");
    result(&env, "commit", "once", true);
    assert_eq!(git(&env.work, &["rev-parse", "HEAD"]), before);
    assert_eq!(git(&linked, &["log", "-1", "--format=%s"]), "Linked worktree commit");
    assert_eq!(git(&linked, &["show", "HEAD:notes.txt"]), "linked content");
}
