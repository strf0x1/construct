//! Fixed-argument host subprocesses. Only this module supplies authentication
//! environment variables; output errors are classified without exposing them.

use crate::sandbox::StopOnDrop;
use crate::tools::{Ctx, ToolError};
mod executable;
pub use executable::Executable;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

struct HostGroup(StopOnDrop);

impl HostGroup {
    fn kill_group(&self) {
        self.0.kill_group();
    }
    fn disarm(&mut self) {
        self.0.0 = None;
    }
}

impl Drop for HostGroup {
    fn drop(&mut self) {
        self.kill_group();
        self.disarm();
    }
}

async fn drain(mut pipe: impl AsyncRead + Unpin, limit: usize) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 16384];
    loop {
        let n = pipe.read(&mut chunk).await?;
        if n == 0 {
            return Ok(bytes);
        }
        let keep = n.min(limit.saturating_sub(bytes.len()));
        bytes.extend_from_slice(&chunk[..keep]);
    }
}

pub fn private_root(ctx: &Ctx) -> Result<PathBuf, ToolError> {
    private_directory(ctx, "developer-tmp")
}

pub(super) fn private_directory(ctx: &Ctx, name: &str) -> Result<PathBuf, ToolError> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    let root = ctx.paths.home.join(name);
    let policy = ctx.policy();
    let writable = |path: &Path| {
        policy.write.iter().any(|entry| path.starts_with(crate::sandbox::Policy::real(entry)))
            || path.starts_with(crate::sandbox::Policy::real(&ctx.cwd))
    };
    if writable(&crate::sandbox::Policy::real(&root)) {
        return Err("authenticated operation state must be outside every shell-writable root".into());
    }
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true).mode(0o700).create(&root).map_err(|_| "cannot create private developer directory")?;
    let meta = std::fs::symlink_metadata(&root).map_err(|_| "cannot inspect private developer directory")?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err("private developer directory must be a real directory".into());
    }
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))
        .map_err(|_| "cannot protect private developer directory")?;
    let real = root.canonicalize().map_err(|_| "cannot resolve private developer directory")?;
    if writable(&real) {
        return Err("authenticated operation state overlaps a shell-writable root".into());
    }
    Ok(real)
}

pub struct Scratch(pub PathBuf);

impl Scratch {
    pub fn new(ctx: &Ctx) -> Result<Self, ToolError> {
        use std::os::unix::fs::DirBuilderExt;
        let path = private_root(ctx)?.join(format!("{}-{}", std::process::id(), crate::skills::now_nanos()));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .map_err(|_| "cannot create private operation directory")?;
        Ok(Self(path))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub async fn run(
    ctx: &Ctx,
    exe: &Executable,
    cwd: &Path,
    args: &[OsString],
    input: &[u8],
    env: &[(&str, OsString)],
) -> Result<Vec<u8>, ToolError> {
    run_limited(ctx, exe, cwd, args, input, env, 2 * 1024 * 1024).await
}

/// [`run`] keeping at most `limit` bytes of output: a bundle for Deck's
/// gateway is larger than any metadata read.
pub async fn run_limited(
    ctx: &Ctx,
    exe: &Executable,
    cwd: &Path,
    args: &[OsString],
    input: &[u8],
    env: &[(&str, OsString)],
    limit: usize,
) -> Result<Vec<u8>, ToolError> {
    exe.check()?;
    if *ctx.cancel.borrow() {
        return Err(ToolError::new("interrupted", "developer operation interrupted before execution"));
    }
    let mut command = tokio::process::Command::new(&exe.path);
    command.env_clear();
    for key in [
        "HOME",
        "LANG",
        "LC_ALL",
        "XDG_CONFIG_HOME",
        "XDG_RUNTIME_DIR",
        "DBUS_SESSION_BUS_ADDRESS",
        "GH_CONFIG_DIR",
        "GH_TOKEN",
        "GITHUB_TOKEN",
        "GH_ENTERPRISE_TOKEN",
        "GITHUB_ENTERPRISE_TOKEN",
        "SSH_AUTH_SOCK",
        "HTTPS_PROXY",
        "HTTP_PROXY",
        "ALL_PROXY",
        "NO_PROXY",
        "https_proxy",
        "http_proxy",
        "all_proxy",
        "no_proxy",
        "SSL_CERT_FILE",
        "SSL_CERT_DIR",
        "CURL_CA_BUNDLE",
        "GIT_SSL_CAINFO",
    ] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    let safe_path = std::env::var_os("PATH").unwrap_or_default();
    let policy = ctx.policy();
    let dirs: Vec<_> = std::env::split_paths(&safe_path)
        .filter_map(|dir| dir.canonicalize().ok())
        .filter(|dir| !dir.starts_with(&ctx.cwd) && !policy.write.iter().any(|root| dir.starts_with(root)))
        .collect();
    command.env("PATH", std::env::join_paths(dirs).map_err(|_| "host PATH could not be sanitized")?);
    let scratch = Scratch::new(ctx)?;
    command.env("TMPDIR", &scratch.0).env("GH_PROMPT_DISABLED", "1").env("GIT_TERMINAL_PROMPT", "0");
    command.env("GIT_PAGER", "cat").env("PAGER", "cat");
    command.env("GIT_NO_REPLACE_OBJECTS", "1");
    command.env("GIT_NO_LAZY_FETCH", "1");
    for (key, value) in env {
        command.env(key, value);
    }
    command.args(args).current_dir(cwd).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    command.process_group(0).kill_on_drop(true);
    let mut child = command.spawn().map_err(|_| ToolError::new("spawn", "could not start approved host executable"))?;
    let mut guard = HostGroup(StopOnDrop(child.id()));
    let stdin = child.stdin.take();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let mut cancel = ctx.cancel.clone();
    let operation = async {
        let write = async {
            if let Some(mut stdin) = stdin {
                stdin.write_all(input).await?;
                stdin.shutdown().await?;
            }
            Ok::<_, std::io::Error>(())
        };
        let read = |pipe: Option<tokio::process::ChildStdout>| async move {
            let mut bytes = Vec::new();
            if let Some(pipe) = pipe {
                bytes = drain(pipe, limit).await?;
            }
            Ok::<_, std::io::Error>(bytes)
        };
        let errors = async {
            let mut bytes = Vec::new();
            if let Some(pipe) = stderr {
                bytes = drain(pipe, 64 * 1024).await?;
            }
            Ok::<_, std::io::Error>(bytes)
        };
        let (written, out, err) = tokio::join!(write, read(stdout), errors);
        let status = child.wait().await?;
        if status.success() {
            written?;
        }
        Ok::<_, std::io::Error>((status, out?, err?))
    };
    let cancelled = async {
        loop {
            if *cancel.borrow() {
                return;
            }
            if cancel.changed().await.is_err() {
                std::future::pending::<()>().await;
            }
        }
    };
    let result = tokio::select! {
        r = tokio::time::timeout(std::time::Duration::from_secs(120), operation) => r,
        _ = cancelled => {
            guard.kill_group();
            return Err(ToolError::new("unknown", "interrupted after host execution started; reconcile the result before repeating this operation"));
        }
    };
    match result {
        Err(_) => {
            guard.kill_group();
            Err(ToolError::new(
                "unknown",
                "host operation timed out; reconcile the result before repeating this operation",
            ))
        }
        Ok(Err(_)) => Err(ToolError::new("unknown", "host operation lost its result; reconcile before repeating")),
        Ok(Ok((status, out, err))) => {
            guard.kill_group();
            guard.disarm();
            if status.success() {
                return Ok(out);
            }
            let error = String::from_utf8_lossy(&err).to_lowercase();
            let (code, message) = if error.contains("authentication")
                || error.contains("credentials")
                || error.contains("bad credentials")
                || error.contains("http 401")
                || error.contains("http 403")
                || error.contains("permission denied (publickey)")
            {
                (
                    "authentication",
                    "Host output indicates an authentication problem outside the tool sandbox. Check the host account or credential helper. The outcome is unconfirmed; reconcile before repeating.",
                )
            } else if error.contains("http 4") || error.contains("[rejected]") || error.contains("[remote rejected]") {
                (
                    "rejected",
                    "Host output indicates that the service rejected the operation. The outcome is unconfirmed; reconcile before repeating.",
                )
            } else if error.contains("operation not permitted") || error.contains("permission denied") {
                (
                    "host_permission",
                    "Host output reports an OS or filesystem permission denial outside the ordinary tool sandbox. The outcome is unconfirmed; reconcile before repeating.",
                )
            } else {
                (
                    "operation_failed",
                    "Approved host operation failed. The outcome is unconfirmed; reconcile before repeating.",
                )
            };
            Err(ToolError::new(code, message).with_data(serde_json::json!({"exit_code":status.code()})))
        }
    }
}

pub fn args(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}
