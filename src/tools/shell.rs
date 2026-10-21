//! Run shell commands. bash without -l (no profile scripts on the hot path),
//! stderr folded into stdout so ordering is kept, its own process group so a
//! timeout or an interrupt kills the whole tree.

use super::Ctx;
use serde_json::Value;
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::io::AsyncReadExt;

const DEFAULT_TIMEOUT_S: u64 = 120;
const MAX_TIMEOUT_S: u64 = 1800;

pub async fn run(ctx: &Ctx, a: &Value) -> Result<String, String> {
    let command = a.get("command").and_then(Value::as_str).ok_or("command is required")?;
    let timeout = a.get("timeout").and_then(Value::as_u64).unwrap_or(DEFAULT_TIMEOUT_S).clamp(1, MAX_TIMEOUT_S);
    let cwd =
        a.get("workdir").and_then(Value::as_str).map(|d| super::fs::resolve(ctx, d)).unwrap_or_else(|| ctx.cwd.clone());
    ctx.sandbox.check_read(&cwd)?;

    let shell = if std::path::Path::new("/bin/bash").exists() { "/bin/bash" } else { "/bin/sh" };
    let mut cmd = ctx.sandbox.command(shell, &format!("exec 2>&1\n{command}"));
    cmd.current_dir(&cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .env("CONSTRUCT", "1")
        .env("GIT_PAGER", "cat")
        .env("PAGER", "cat")
        .kill_on_drop(true)
        .process_group(0);
    let started = Instant::now();
    let mut child = cmd.spawn().map_err(|e| format!("spawn failed: {e}"))?;
    let pid = child.id();
    let mut stdout = child.stdout.take().unwrap();
    let mut cancel = ctx.cancel.clone();

    let mut buf = Vec::new();
    let mut chunk = [0u8; 16384];
    let deadline = tokio::time::sleep(Duration::from_secs(timeout));
    tokio::pin!(deadline);
    let mut stopped: Option<&str> = None;
    loop {
        tokio::select! {
            n = stdout.read(&mut chunk) => match n {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    // Keep memory bounded on runaway output: head + rolling tail.
                    buf.extend_from_slice(&chunk[..n]);
                    if buf.len() > 8 * 1024 * 1024 {
                        let keep = buf.split_off(buf.len() - 4 * 1024 * 1024);
                        buf.truncate(64 * 1024);
                        buf.extend_from_slice(b"\n[... output dropped ...]\n");
                        buf.extend_from_slice(&keep);
                    }
                }
            },
            _ = &mut deadline => { stopped = Some("timed out"); break; }
            _ = cancel.changed() => { if *cancel.borrow() { stopped = Some("interrupted"); break; } }
        }
    }
    if stopped.is_some()
        && let Some(pid) = pid
    {
        // SAFETY: plain syscall on a process group we created.
        unsafe { libc::kill(-(pid as i32), libc::SIGKILL) };
    }
    let status = child.wait().await.ok();
    let elapsed = started.elapsed();
    let text = String::from_utf8_lossy(&buf).to_string();
    let mut out = super::cap_output(ctx, "shell", &text);
    if out.is_empty() {
        out.push_str("(no output)");
    }
    let code = status.and_then(|s| s.code());
    match (stopped, code) {
        (Some(why), _) => Err(format!("{out}\n[{why} after {:.1}s]", elapsed.as_secs_f32())),
        (None, Some(0)) => Ok(out),
        (None, Some(c)) => {
            // Landlock denials look like ordinary permission errors; say why.
            let hint =
                if !ctx.sandbox.off && (out.contains("Permission denied") || out.contains("Operation not permitted")) {
                    "\n[sandbox: commands can only reach the workspace, system paths and toolchains]"
                } else {
                    ""
                };
            Err(format!("{out}\n[exit {c}]{hint}"))
        }
        (None, None) => Err(format!("{out}\n[killed by signal]")),
    }
}
