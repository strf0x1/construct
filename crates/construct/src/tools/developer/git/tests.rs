//! Initial commits use the same frozen-tree approval as later commits, with
//! an absent branch ref compared atomically when the commit is installed.

use super::super::prepare;
use crate::config::{MemoryConfig, Paths, SandboxConfig};
use crate::memory::Store;
use crate::sandbox::Policy;
use crate::tools::Ctx;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::Command;

fn git(work: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(work)
        .args(["-c", "core.hooksPath=/dev/null", "-c", "core.fsmonitor=false", "-c", "commit.gpgsign=false"])
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8(output.stdout).unwrap().trim_end().to_owned()
}

fn context(name: &str, format: &str) -> (Ctx, PathBuf) {
    let root = std::env::temp_dir().join(format!("construct-initial-commit-{name}-{}", crate::skills::now_nanos()));
    let work = root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let root = root.canonicalize().unwrap();
    let work = root.join("work");
    git(&work, &["init", "--initial-branch=main", &format!("--object-format={format}")]);
    git(&work, &["config", "user.name", "Fixture"]);
    git(&work, &["config", "user.email", "fixture@example.invalid"]);
    std::fs::write(work.join("first.txt"), "initial content\n").unwrap();
    git(&work, &["add", "first.txt"]);
    let paths = Paths { home: root.join("home") };
    let store = Store::new(&paths, &MemoryConfig::default());
    let policy = Policy::new(&SandboxConfig::default(), &work, &paths);
    let ctx = Ctx::new(work, paths, store, tokio::sync::watch::channel(false).1, None, policy);
    (ctx, root)
}

#[tokio::test]
async fn initial_commit_records_reviewed_tree_without_a_parent_in_both_object_formats() {
    for format in ["sha1", "sha256"] {
        let (ctx, root) = context("initial", format);
        let plan = prepare::prepare(&ctx, json!({"operation":"commit","message":"Initial commit"})).await.unwrap();
        assert!(plan.request.detail.contains("Parent: none (initial commit)"));
        assert!(plan.request.detail.contains("+initial content"));
        let output = plan.execute(&ctx).await.unwrap();
        assert!(output.text.starts_with("Committed "));
        assert_eq!(git(&ctx.cwd, &["show", "HEAD:first.txt"]), "initial content");
        assert_eq!(git(&ctx.cwd, &["rev-list", "--count", "HEAD"]), "1");
        assert_eq!(git(&ctx.cwd, &["log", "-1", "--format=%P"]), "");
        assert_eq!(git(&ctx.cwd, &["log", "-1", "--format=%B"]), "Initial commit");
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[tokio::test]
async fn a_branch_created_while_initial_commit_waits_is_not_overwritten() {
    let (ctx, root) = context("changed", "sha1");
    let plan = prepare::prepare(&ctx, json!({"operation":"commit","message":"Approved initial commit"})).await.unwrap();
    git(&ctx.cwd, &["commit", "-m", "Other initial commit"]);
    let existing = git(&ctx.cwd, &["rev-parse", "HEAD"]);
    assert_eq!(plan.execute(&ctx).await.err().unwrap().code, "changed");
    assert_eq!(git(&ctx.cwd, &["rev-parse", "HEAD"]), existing);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn corrupt_branch_is_not_treated_as_an_initial_commit() {
    let (ctx, root) = context("corrupt", "sha1");
    std::fs::write(ctx.cwd.join(".git/refs/heads/main"), "invalid-object\n").unwrap();
    assert!(
        prepare::prepare(&ctx, json!({"operation":"commit","message":"Must not overwrite corruption"})).await.is_err()
    );
    assert_eq!(std::fs::read_to_string(ctx.cwd.join(".git/refs/heads/main")).unwrap(), "invalid-object\n");
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn configured_reference_transaction_hooks_cannot_run_during_approved_commit() {
    let (ctx, root) = context("configured-hook", "sha1");
    let marker = root.join("configured-hook-ran");
    git(&ctx.cwd, &["config", "hook.attack.command", &format!("touch '{}'; exit 1", marker.display())]);
    git(&ctx.cwd, &["config", "hook.attack.event", "reference-transaction"]);
    let plan = prepare::prepare(&ctx, json!({"operation":"commit","message":"Approved without hooks"})).await.unwrap();
    assert!(plan.execute(&ctx).await.unwrap().text.starts_with("Committed "));
    assert!(!marker.exists(), "repository hook configuration reached host execution");
    assert_eq!(git(&ctx.cwd, &["show", "HEAD:first.txt"]), "initial content");
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn isolated_commit_context_preserves_reftable_storage() {
    let (ctx, root) = context("reftable", "sha1");
    git(&ctx.cwd, &["refs", "migrate", "--ref-format=reftable"]);
    let plan = prepare::prepare(&ctx, json!({"operation":"commit","message":"Initial reftable commit"})).await.unwrap();
    assert!(plan.execute(&ctx).await.unwrap().text.starts_with("Committed "));
    assert_eq!(git(&ctx.cwd, &["show", "HEAD:first.txt"]), "initial content");
    assert_eq!(git(&ctx.cwd, &["rev-parse", "--show-ref-format"]), "reftable");
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn preparing_a_partial_clone_cannot_start_repository_selected_ssh_for_lazy_fetch() {
    let (ctx, root) = context("lazy-fetch", "sha1");
    git(&ctx.cwd, &["commit", "-m", "Source commit"]);
    let blob = git(&ctx.cwd, &["rev-parse", "HEAD:first.txt"]);
    std::fs::write(ctx.cwd.join("first.txt"), "changed content\n").unwrap();
    git(&ctx.cwd, &["add", "first.txt"]);
    git(&ctx.cwd, &["config", "core.repositoryformatversion", "1"]);
    git(&ctx.cwd, &["config", "extensions.partialclone", "origin"]);
    git(&ctx.cwd, &["config", "remote.origin.url", "ssh://git@example.invalid/source.git"]);
    git(&ctx.cwd, &["config", "remote.origin.promisor", "true"]);
    let marker = root.join("lazy-fetch-helper-ran");
    git(&ctx.cwd, &["config", "core.sshCommand", &format!("touch '{}'; exit 1", marker.display())]);
    std::fs::remove_file(ctx.cwd.join(".git/objects").join(blob.get(..2).unwrap()).join(blob.get(2..).unwrap()))
        .unwrap();
    assert!(prepare::prepare(&ctx, json!({"operation":"commit","message":"Must not fetch"})).await.is_err());
    assert!(!marker.exists(), "preparing a review ran a repository-selected network helper");
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn preparing_and_committing_never_run_index_hooks_or_rewrite_the_original_index() {
    let (ctx, root) = context("index-hooks", "sha1");
    let configured = root.join("configured-index-hook-ran");
    let file_hook = root.join("file-index-hook-ran");
    git(&ctx.cwd, &["config", "hook.attack.command", &format!("touch '{}'; exit 1", configured.display())]);
    git(&ctx.cwd, &["config", "hook.attack.event", "post-index-change"]);
    let hook = ctx.cwd.join(".git/hooks/post-index-change");
    std::fs::write(&hook, format!("#!/bin/sh\ntouch '{}'\n", file_hook.display())).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o700)).unwrap();
    let index = std::fs::read(ctx.cwd.join(".git/index")).unwrap();
    let plan = prepare::prepare(&ctx, json!({"operation":"commit","message":"Review index privately"})).await.unwrap();
    assert!(!configured.exists() && !file_hook.exists());
    assert_eq!(std::fs::read(ctx.cwd.join(".git/index")).unwrap(), index);
    assert!(plan.execute(&ctx).await.unwrap().text.starts_with("Committed "));
    assert!(!configured.exists() && !file_hook.exists());
    assert_eq!(std::fs::read(ctx.cwd.join(".git/index")).unwrap(), index);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn private_review_copies_split_index_backing_without_mutating_it() {
    let (ctx, root) = context("split-index", "sha1");
    git(&ctx.cwd, &["update-index", "--split-index"]);
    let index = std::fs::read(ctx.cwd.join(".git/index")).unwrap();
    let plan = prepare::prepare(&ctx, json!({"operation":"commit","message":"Split index commit"})).await.unwrap();
    assert_eq!(std::fs::read(ctx.cwd.join(".git/index")).unwrap(), index);
    assert!(plan.execute(&ctx).await.unwrap().text.starts_with("Committed "));
    assert_eq!(git(&ctx.cwd, &["show", "HEAD:first.txt"]), "initial content");
    assert_eq!(std::fs::read(ctx.cwd.join(".git/index")).unwrap(), index);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn committing_a_linked_worktree_updates_its_head_and_branch_reflogs_only() {
    let (main, root) = context("linked-reflog", "sha1");
    git(&main.cwd, &["commit", "-m", "Initial main commit"]);
    let linked = root.join("linked");
    git(&main.cwd, &["worktree", "add", "-b", "feature", linked.to_str().unwrap()]);
    let before = git(&main.cwd, &["reflog", "show", "HEAD", "--format=%H:%gs"]);
    std::fs::write(linked.join("first.txt"), "feature content\n").unwrap();
    git(&linked, &["add", "first.txt"]);
    let paths = Paths { home: root.join("linked-home") };
    let store = Store::new(&paths, &MemoryConfig::default());
    let policy = Policy::new(&SandboxConfig::default(), &linked, &paths);
    let ctx = Ctx::new(linked, paths, store, tokio::sync::watch::channel(false).1, None, policy);
    let plan = prepare::prepare(&ctx, json!({"operation":"commit","message":"Linked feature commit"})).await.unwrap();
    assert!(plan.execute(&ctx).await.unwrap().text.starts_with("Committed "));
    let head = git(&ctx.cwd, &["rev-parse", "HEAD"]);
    assert_eq!(git(&ctx.cwd, &["reflog", "-1", "--format=%H", "HEAD"]), head);
    assert_eq!(git(&ctx.cwd, &["reflog", "-1", "--format=%H", "refs/heads/feature"]), head);
    assert_eq!(git(&main.cwd, &["reflog", "show", "HEAD", "--format=%H:%gs"]), before);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn reference_directory_symlinks_cannot_redirect_an_approved_commit() {
    use std::os::unix::fs::symlink;
    let (ctx, root) = context("redirected-refs", "sha1");
    git(&ctx.cwd, &["commit", "-m", "Initial commit"]);
    std::fs::write(ctx.cwd.join("first.txt"), "next content\n").unwrap();
    git(&ctx.cwd, &["add", "first.txt"]);
    let heads = ctx.cwd.join(".git/refs/heads");
    let external = root.join("outside-heads");
    std::fs::rename(&heads, &external).unwrap();
    symlink(&external, &heads).unwrap();
    let before = std::fs::read(external.join("main")).unwrap();
    assert!(prepare::prepare(&ctx, json!({"operation":"commit","message":"Must not redirect refs"})).await.is_err());
    assert_eq!(std::fs::read(external.join("main")).unwrap(), before);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn existing_and_dangling_head_log_symlinks_cannot_redirect_host_writes() {
    use std::os::unix::fs::symlink;
    for existing in [false, true] {
        let (ctx, root) = context("redirected-log", "sha1");
        let logs = ctx.cwd.join(".git/logs");
        std::fs::create_dir_all(&logs).unwrap();
        let outside = root.join("outside-log");
        if existing {
            std::fs::write(&outside, "keep this\n").unwrap();
        }
        symlink(&outside, logs.join("HEAD")).unwrap();
        assert!(
            prepare::prepare(&ctx, json!({"operation":"commit","message":"Must not redirect logs"})).await.is_err()
        );
        if existing {
            assert_eq!(std::fs::read_to_string(&outside).unwrap(), "keep this\n");
        } else {
            assert!(!outside.exists());
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[tokio::test]
async fn loose_object_prefix_symlinks_are_rejected_before_preparing_a_tree() {
    use std::os::unix::fs::symlink;
    let (ctx, root) = context("redirected-objects", "sha1");
    let objects = ctx.cwd.join(".git/objects");
    let prefix = (0..=255).map(|value| format!("{value:02x}")).find(|name| !objects.join(name).exists()).unwrap();
    let outside = root.join("outside-objects");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("sentinel"), "unchanged\n").unwrap();
    symlink(&outside, objects.join(prefix)).unwrap();
    let index = std::fs::read(ctx.cwd.join(".git/index")).unwrap();
    assert!(prepare::prepare(&ctx, json!({"operation":"commit","message":"Must not redirect objects"})).await.is_err());
    assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 1);
    assert_eq!(std::fs::read_to_string(outside.join("sentinel")).unwrap(), "unchanged\n");
    assert_eq!(std::fs::read(ctx.cwd.join(".git/index")).unwrap(), index);
    std::fs::remove_dir_all(root).unwrap();
}
