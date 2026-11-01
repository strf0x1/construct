//! Security checks use real repository discovery and a recording executor,
//! so no authenticated request or network connection leaves the test.

use super::{prepare, runner};
use crate::config::{MemoryConfig, Paths, SandboxConfig};
use crate::memory::Store;
use crate::sandbox::Policy;
use crate::tools::Ctx;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::Command;

fn git(work: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(["-c", "core.hooksPath=/dev/null", "-c", "commit.gpgsign=false", "-c", "core.fsmonitor=false"])
        .arg("-C")
        .arg(work)
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
}

fn context(tag: &str, home_in_workspace: bool) -> (Ctx, PathBuf) {
    let root = std::env::temp_dir().join(format!("construct-developer-safety-{tag}-{}", crate::skills::now_nanos()));
    std::fs::create_dir_all(root.join("work")).unwrap();
    let root = root.canonicalize().unwrap();
    let work = root.join("work");
    let paths = Paths { home: if home_in_workspace { work.join("construct-home") } else { root.join("home") } };
    let store = Store::new(&paths, &MemoryConfig::default());
    let policy = Policy::new(&SandboxConfig::default(), &work, &paths);
    let ctx = Ctx::new(work, paths, store, tokio::sync::watch::channel(false).1, None, policy);
    (ctx, root)
}

#[tokio::test]
async fn repository_discovery_cannot_replace_the_allowed_worktree_with_an_external_one() {
    let (ctx, root) = context("worktree", false);
    let outside = root.join("outside");
    std::fs::create_dir(&outside).unwrap();
    git(&outside, &["init"]);
    git(&outside, &["config", "core.worktree", outside.to_str().unwrap()]);
    std::fs::write(ctx.cwd.join(".git"), format!("gitdir: {}\n", outside.join(".git").display())).unwrap();
    let error = match prepare::prepare(&ctx, json!({"operation":"commit","message":"must not inspect outside"})).await {
        Err(error) => error,
        Ok(_) => panic!("an external worktree was accepted"),
    };
    assert_eq!(error.code, "sandbox", "{error}");
    assert!(!outside.join(".git/index").exists(), "no external index preparation should run");
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn authenticated_scratch_cannot_share_a_root_writable_by_ordinary_shell_commands() {
    let (ctx, root) = context("scratch", true);
    assert!(runner::private_root(&ctx).is_err(), "workspace-writable host scratch permits input substitution");
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn push_does_not_copy_repository_credential_helpers_into_host_execution() {
    use std::os::unix::fs::PermissionsExt;
    let (ctx, root) = context("helper", false);
    git(&ctx.cwd, &["init"]);
    git(
        &ctx.cwd,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "--allow-empty",
            "-m",
            "fixture",
        ],
    );
    git(&ctx.cwd, &["branch", "-M", "fixture"]);
    git(&ctx.cwd, &["remote", "add", "origin", "https://example.invalid/owner/repo.git"]);
    let helper = format!("!touch {}", root.join("repository-helper-ran").display());
    git(&ctx.cwd, &["config", "credential.helper", &helper]);
    let mut plan =
        prepare::prepare(&ctx, json!({"operation":"push","remote":"origin","branch":"fixture"})).await.unwrap();
    let executable = root.join("recording-git");
    let record = root.join("recorded-host-config");
    let args = root.join("recorded-args");
    std::fs::write(
        &executable,
        format!(
            "#!/bin/sh\ncat \"$GIT_CONFIG_GLOBAL\" > '{}'\nprintf '%s\\n' \"$@\" > '{}'\n",
            record.display(),
            args.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    plan.executable = runner::Executable::fake(executable);
    let output = plan.execute(&ctx).await.unwrap();
    assert!(output.text.starts_with("Pushed "), "{}", output.text);
    let config = std::fs::read_to_string(record).unwrap();
    assert!(!config.contains(&helper), "repository helper escaped into host configuration");
    assert!(!config.contains("repository-helper-ran"));
    assert!(!root.join("repository-helper-ran").exists());
    let invocation = std::fs::read_to_string(args).unwrap();
    assert!(invocation.contains("https://example.invalid/owner/repo.git\n"), "{invocation}");
    assert!(invocation.contains(":refs/heads/fixture\n"), "{invocation}");
    std::fs::remove_dir_all(root).unwrap();
}
