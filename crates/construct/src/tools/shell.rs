//! Run shell commands. bash without -l (no profile scripts on the hot path),
//! stderr folded into stdout so ordering is kept. Every call runs under the
//! sandbox helper, which kills whatever the command left running when it
//! ends; a timeout or an interrupt asks the helper to stop (SIGTERM) so it can
//! sweep, and only then kills the process group. The helper's stderr is its
//! report of what it stopped.

use super::{Ctx, Display, Group, Output, OutputKind, Tool, ToolError, ToolKind, def, obj};
use crate::permission::{Access, blocked_in};
use crate::sandbox::{GRACE, Policy, SWEPT, StopOnDrop};
use serde_json::{Value, json};
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::io::AsyncReadExt;

const DEFAULT_TIMEOUT_S: u64 = 120;
const MAX_TIMEOUT_S: u64 = 1800;

pub struct Shell;

impl Shell {
    pub const NAME: &str = "shell";
}

#[async_trait::async_trait]
impl Tool for Shell {
    fn name(&self) -> &'static str {
        Self::NAME
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Execute
    }
    fn group(&self) -> Group {
        Group::Core
    }
    fn definition(&self) -> Value {
        def(
            Self::NAME,
            "Run a bash command and return its combined stdout/stderr. Use for builds, tests, git, and anything without a dedicated tool. Default timeout 120s.",
            obj(
                json!({
                    "command": { "type": "string" },
                    "workdir": { "type": "string", "description": "Directory to run in; defaults to the session's" },
                    "timeout": { "type": "integer", "description": "Seconds, max 1800" }
                }),
                &["command"],
            ),
        )
    }
    fn display(&self) -> Display {
        Display { primary_arg: "command", preview_lines: 4 }
    }
    async fn call(&self, ctx: &Ctx, a: Value) -> Result<Output, ToolError> {
        run(ctx, &a).await.map(|s| Output::plain(s).with_kind(OutputKind::Tail)).map_err(|mut e| {
            e.kind = OutputKind::Tail;
            e
        })
    }
}

/// Most times one call may be run again after the user widens the sandbox for
/// it: a read refused, then the write that follows it.
const MAX_ASKS: usize = 2;

async fn run(ctx: &Ctx, a: &Value) -> Result<String, ToolError> {
    let command = a.get("command").and_then(Value::as_str).ok_or("command is required")?;
    let timeout = a.get("timeout").and_then(Value::as_u64).unwrap_or(DEFAULT_TIMEOUT_S).clamp(1, MAX_TIMEOUT_S);
    let cwd =
        a.get("workdir").and_then(Value::as_str).map(|d| super::fs::resolve(ctx, d)).unwrap_or_else(|| ctx.cwd.clone());
    ctx.can_path(Shell::NAME, Access::Read, &cwd).await?;
    let mut policy = ctx.policy();
    let mut asks = 0;
    loop {
        let r = run_once(ctx, &policy, command, &cwd, timeout).await;
        // A Landlock denial is only visible in the command's own output, so
        // asking comes after the fact: the user sees which paths it named and
        // that approving runs the command again.
        let Err(e) = &r else { return r };
        if e.code != "exit" || asks == MAX_ASKS {
            return r;
        }
        let Some((access, paths)) = blocked_in(&policy, &e.message) else { return r };
        let Some(extra) = ctx.perms.shell(command, access, paths, ctx.cancel.clone()).await else { return r };
        policy = match access {
            Access::Read => policy.widened(&extra, &[]),
            Access::Write => policy.widened(&[], &extra),
        };
        asks += 1;
    }
}

async fn run_once(
    ctx: &Ctx,
    policy: &Policy,
    command: &str,
    cwd: &std::path::Path,
    timeout: u64,
) -> Result<String, ToolError> {
    let shell = if std::path::Path::new("/bin/bash").exists() { "/bin/bash" } else { "/bin/sh" };
    let mut cmd = policy.command(shell, &format!("exec 2>&1\n{command}"));
    cmd.current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("CONSTRUCT", "1")
        .env("GIT_PAGER", "cat")
        .env("PAGER", "cat")
        .process_group(0);
    let started = Instant::now();
    let mut child = cmd.spawn().map_err(|e| format!("spawn failed: {e}"))?;
    let mut guard = StopOnDrop(child.id());
    let (Some(mut stdout), Some(mut stderr)) = (child.stdout.take(), child.stderr.take()) else {
        return Err("spawn failed: no pipes".into());
    };
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
    if stopped.is_some() {
        guard.stop();
    }
    let status = match tokio::time::timeout(GRACE, child.wait()).await {
        Ok(s) => s.ok(),
        Err(_) => {
            guard.kill_group();
            child.wait().await.ok()
        }
    };
    guard.0 = None;
    let elapsed = started.elapsed();
    let mut report = Vec::new();
    let _ = tokio::time::timeout(GRACE, stderr.read_to_end(&mut report)).await;
    let (swept, helper) = parse_report(&String::from_utf8_lossy(&report));
    let text = String::from_utf8_lossy(&buf).to_string();
    let mut out = super::cap_output(ctx, "shell", &text);
    out.push_str(&helper);
    if out.is_empty() {
        out.push_str("(no output)");
    }
    if !swept.is_empty() {
        let n = swept.len();
        let mut names = swept;
        names.dedup();
        let s = if n == 1 { "" } else { "es" };
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&format!("[stopped {n} background process{s}: {}]", names.join(", ")));
    }
    let code = status.and_then(|s| s.code());
    match (stopped, code) {
        (Some(why), _) => Err(ToolError::new(
            if why == "timed out" { "timeout" } else { "interrupted" },
            format!("{out}\n[{why} after {:.1}s]", elapsed.as_secs_f32()),
        )),
        (None, Some(0)) => Ok(out),
        (None, Some(c)) => {
            // Landlock denials look like ordinary permission errors; say why.
            let hint = if !policy.off && (out.contains("Permission denied") || out.contains("Operation not permitted"))
            {
                "\n[sandbox: commands can only reach the workspace, system paths and toolchains]"
            } else {
                ""
            };
            Err(ToolError::new("exit", format!("{out}\n[exit {c}]{hint}")))
        }
        (None, None) => Err(ToolError::new("signal", format!("{out}\n[killed by signal]"))),
    }
}

/// Split the helper's stderr into the names it swept, sorted, and anything
/// else it said (a sandbox failure).
fn parse_report(report: &str) -> (Vec<String>, String) {
    let mut swept = Vec::new();
    let mut other = String::new();
    for line in report.lines() {
        match line.strip_prefix(SWEPT).and_then(|r| r.trim_start().split_once(' ')) {
            Some((_pid, comm)) => swept.push(comm.to_string()),
            None => {
                other.push('\n');
                other.push_str(line);
            }
        }
    }
    swept.sort();
    (swept, other)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_separates_swept_names_from_helper_messages() {
        let r = format!("{SWEPT} 12 sleep\n{SWEPT} 9 bf\nconstruct sandbox: landlock: nope\n");
        let (swept, other) = parse_report(&r);
        assert_eq!(swept, ["bf", "sleep"]);
        assert_eq!(other, "\nconstruct sandbox: landlock: nope");
    }
}
