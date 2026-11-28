//! Give a fixed Git process its own configuration and access to native
//! worktree metadata. The regular shell retains its original sandbox.

use super::super::{git, options, ref_format};
use crate::developer::runner::{self, Executable, Scratch};
use construct_core::sandbox::{GRACE, Policy, StopOnDrop};
use construct_core::tools::{Ctx, ToolError};
use serde_json::json;
use std::io::Read as _;
use std::os::unix::fs::{OpenOptionsExt as _, symlink};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncWriteExt as _;

pub(super) struct Checkout {
    pub root: PathBuf,
    git_dir: PathBuf,
    pub oid_len: usize,
    exe: Executable,
    scratch: Scratch,
    policy: Policy,
}

impl Checkout {
    pub(super) async fn open(ctx: &Ctx, write: bool) -> Result<Self, ToolError> {
        let base = ctx.policy();
        let root = ctx.cwd().canonicalize().map_err(|_| "cannot resolve the current checkout")?;
        base.check_read(&root).map_err(ToolError::sandbox)?;
        if write {
            base.check_write(&root).map_err(ToolError::sandbox)?;
        }
        let git_dir = native_dir(&root)?;
        let common = git_dir.join(pointer(&git_dir.join("commondir"), "")?);
        let common = common.canonicalize().map_err(|_| "cannot resolve the common Git directory")?;
        if git_dir.parent() != Some(common.join("worktrees").as_path())
            || [&common, &git_dir].into_iter().any(|path| {
                construct_core::permission::protected(path)
                    || path.starts_with(Policy::real(ctx.home()))
                    || base.write.iter().any(|writable| path.starts_with(writable))
            })
        {
            return Err("The linked Git metadata is outside the supported worktree layout.".into());
        }
        let objects = common.join("objects");
        if objects.canonicalize().ok().as_ref() != Some(&objects) {
            return Err("The repository needs a native Git object directory.".into());
        }
        let exe = Executable::find("git", ctx)?;
        let refs = ref_format(ctx, &exe, &root).await?;
        let oid_len = match git(ctx, &exe, &root, &["rev-parse", "--show-object-format"]).await?.as_str() {
            "sha1" => 40,
            "sha256" => 64,
            _ => return Err("unsupported Git object format".into()),
        };
        let branch = git(ctx, &exe, &root, &["symbolic-ref", "-q", "HEAD"]).await?;
        if !branch.starts_with("refs/heads/") || native_dir(&root)? != git_dir {
            return Err("These Git operations need the linked worktree's own branch.".into());
        }
        let scratch = Scratch::new(ctx)?;
        let mut read = vec![root.clone(), git_dir.clone(), objects.clone(), scratch.0.clone()];
        for name in ["refs", "packed-refs", "reftable"] {
            let store = common.join(name);
            match store.symlink_metadata() {
                Ok(_) if store.canonicalize().ok().as_ref() == Some(&store) => {
                    symlink(&store, scratch.0.join(name)).map_err(|_| "cannot configure read-only Git refs")?;
                    read.push(store);
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                _ => return Err("The repository needs native Git reference storage.".into()),
            }
        }
        symlink(&objects, scratch.0.join("objects")).map_err(|_| "cannot configure Git objects")?;
        let config = config(ctx, &exe, &root, oid_len, &refs).await?;
        std::fs::write(scratch.0.join("config"), config).map_err(|_| "cannot configure scoped Git")?;
        let tmp = scratch.0.join("tmp");
        std::fs::create_dir(&tmp).map_err(|_| "cannot create scoped Git temp directory")?;
        let mut writable = vec![tmp.clone(), PathBuf::from("/dev/null")];
        if write {
            writable.extend([root.clone(), git_dir.clone(), objects]);
        }
        let mut system = base.system;
        let exe = sandbox_git(ctx, exe).await?;
        system.push(exe.path.clone());
        let policy = Policy {
            read,
            write: writable,
            system,
            tmp,
            parent: std::process::id(),
            no_network: true,
            own_stderr: true,
            ..Policy::default()
        };
        Ok(Self { root, git_dir, oid_len, exe, scratch, policy })
    }

    /// Whether a merge, cherry-pick, revert, rebase or sequencer run is
    /// already under way in this checkout: its state files sit in `git_dir`.
    pub(super) fn operation_in_progress(&self) -> bool {
        ["MERGE_HEAD", "CHERRY_PICK_HEAD", "REVERT_HEAD", "rebase-apply", "rebase-merge", "sequencer"]
            .iter()
            .any(|name| self.git_dir.join(name).symlink_metadata().is_ok())
    }

    pub(super) async fn git(&self, ctx: &Ctx, args: &[&str], input: &[u8]) -> Result<String, ToolError> {
        self.exe.check()?;
        if native_dir(&self.root)? != self.git_dir {
            return Err("The linked checkout changed while preparing the Git operation.".into());
        }
        let exe = self.exe.path.to_str().ok_or("Git executable path is not UTF-8")?;
        let mut argv = options();
        argv.extend(runner::args(args));
        let mut command = self.policy.spawn(exe, argv);
        command
            .current_dir(&self.root)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &self.policy.tmp)
            .env("TMPDIR", &self.policy.tmp)
            .env("LC_ALL", "C")
            .env("GIT_DIR", &self.git_dir)
            .env("GIT_COMMON_DIR", &self.scratch.0)
            .env("GIT_WORK_TREE", &self.root)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_ATTR_NOSYSTEM", "1")
            .env("GIT_NO_REPLACE_OBJECTS", "1")
            .env("GIT_NO_LAZY_FETCH", "1")
            .env("GIT_ALLOW_PROTOCOL", "")
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        run(ctx, command, input).await
    }
}

fn native_dir(root: &Path) -> Result<PathBuf, ToolError> {
    let dotgit = root.join(".git");
    let target = root.join(pointer(&dotgit, "gitdir: ")?);
    let git_dir = target.canonicalize().map_err(|_| "cannot resolve the linked worktree's Git directory")?;
    let back = git_dir.join(pointer(&git_dir.join("gitdir"), "")?);
    if back.canonicalize().ok().as_ref() != Some(&dotgit) {
        return Err("Git has no matching registration for this checkout.".into());
    }
    Ok(git_dir)
}

fn pointer(path: &Path, prefix: &str) -> Result<PathBuf, ToolError> {
    let invalid = || ToolError::new("invalid_worktree", "This operation needs a registered linked Git worktree.");
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| invalid())?;
    let metadata = file.metadata().map_err(|_| invalid())?;
    if !metadata.is_file() || metadata.len() > 8192 {
        return Err(invalid());
    }
    let mut raw = String::new();
    file.take(8193).read_to_string(&mut raw).map_err(|_| invalid())?;
    let path =
        raw.trim_end_matches(['\n', '\r']).strip_prefix(prefix).filter(|path| !path.is_empty()).ok_or_else(invalid)?;
    if raw.len() > 8192 || path.contains(['\0', '\n', '\r']) {
        return Err(invalid());
    }
    Ok(PathBuf::from(path))
}

async fn sandbox_git(ctx: &Ctx, exe: Executable) -> Result<Executable, ToolError> {
    #[cfg(target_os = "macos")]
    if exe.path == Path::new("/usr/bin/git") {
        // The system shim consults Xcode license and cache state. Resolve the
        // actual executable before restricting the fixed Git operation.
        let mut command = tokio::process::Command::new("/usr/bin/xcrun");
        command.args(["--find", "git"]).env_clear().kill_on_drop(true);
        if let Some(developer_dir) = std::env::var_os("DEVELOPER_DIR") {
            command.env("DEVELOPER_DIR", developer_dir);
        }
        let output = tokio::time::timeout(Duration::from_secs(10), command.output())
            .await
            .map_err(|_| "timed out locating Apple Git")?
            .map_err(|_| "could not locate Apple Git")?;
        if !output.status.success() {
            return Err("xcrun could not locate Apple Git".into());
        }
        let path = std::str::from_utf8(&output.stdout).map_err(|_| "Apple Git path is not UTF-8")?;
        return Executable::selected(PathBuf::from(path.trim_end()), ctx);
    }
    let _ = ctx;
    Ok(exe)
}

async fn config(ctx: &Ctx, exe: &Executable, root: &Path, oid_len: usize, refs: &str) -> Result<String, ToolError> {
    let mut config = "[core]\nrepositoryformatversion = 1\nbare = false\nlogallrefupdates = false\n".to_owned();
    let settings = git(
        ctx,
        exe,
        root,
        &["config", "--type=bool", "--get-regexp", r"^core\.(filemode|ignorecase|symlinks|sparsecheckout)$"],
    )
    .await;
    let settings = match settings {
        Ok(settings) => settings,
        Err(error) if error.data.as_ref().and_then(|data| data["exit_code"].as_i64()) == Some(1) => String::new(),
        Err(error) => return Err(error),
    };
    for line in settings.lines() {
        match line.split_once(' ') {
            Some(("core.sparsecheckout", "true")) => return Err("This operation needs a full Git checkout.".into()),
            Some(("core.sparsecheckout", "false")) => {}
            Some((key @ ("core.filemode" | "core.ignorecase" | "core.symlinks"), value @ ("true" | "false"))) => {
                use std::fmt::Write as _;
                let _ = writeln!(config, "{} = {value}", key.trim_start_matches("core."));
            }
            _ => return Err("unsupported Git working tree configuration".into()),
        }
    }
    if oid_len == 64 || refs == "reftable" {
        config.push_str("[extensions]\n");
        if oid_len == 64 {
            config.push_str("objectFormat = sha256\n");
        }
        if refs == "reftable" {
            config.push_str("refStorage = reftable\n");
        }
    }
    config.push_str("[user]\nname = Construct\nemail = construct@localhost\n");
    Ok(config)
}

async fn run(ctx: &Ctx, mut command: tokio::process::Command, input: &[u8]) -> Result<String, ToolError> {
    if ctx.is_cancelled() {
        return Err(ToolError::new("interrupted", "Git operation interrupted before starting."));
    }
    let mut child = command.spawn().map_err(|error| format!("starting scoped Git: {error}"))?;
    let mut guard = StopOnDrop(child.id());
    let (Some(mut stdin), Some(stdout), Some(stderr)) = (child.stdin.take(), child.stdout.take(), child.stderr.take())
    else {
        return Err("could not open scoped Git pipes".into());
    };
    let wait = async {
        let send = async {
            let result = stdin.write_all(input).await;
            drop(stdin);
            result
        };
        const LIMIT: usize = 1024 * 1024 + 1;
        let (sent, out, err, status) =
            tokio::join!(send, runner::drain(stdout, LIMIT), runner::drain(stderr, LIMIT), child.wait());
        let status = status?;
        if status.success() {
            sent?;
        }
        Ok::<_, std::io::Error>((status, out?, err?))
    };
    let mut cancel = ctx.cancel_signal();
    let result = tokio::select! {
        result = tokio::time::timeout(Duration::from_secs(120), wait) => result.ok(),
        _ = cancel.wait_for(|stop| *stop) => None,
    };
    let Some(result) = result else {
        guard.stop();
        if tokio::time::timeout(GRACE, child.wait()).await.is_err() {
            guard.kill_group();
        }
        return Err(ToolError::new(
            "interrupted",
            "Git operation interrupted or timed out. Inspect status before retrying.",
        ));
    };
    guard.0 = None;
    let (status, out, err) = result.map_err(|error| format!("reading scoped Git output: {error}"))?;
    if status.success() {
        if out.len() > 1024 * 1024 {
            return Err("Git output exceeded 1 MiB.".into());
        }
        return Ok(String::from_utf8_lossy(&out).into_owned());
    }
    let message = String::from_utf8_lossy(if err.is_empty() { &out } else { &err });
    Err(ToolError::new("git", ctx.cap_output("git", &message)).with_data(json!({"exit_code":status.code()})))
}
