//! Run shell commands. bash without -l (no profile scripts on the hot path),
//! stderr folded into stdout so ordering is kept. Every call runs under the
//! sandbox helper, which kills whatever the command left running when it
//! ends; a timeout or an interrupt asks the helper to stop (SIGTERM) so it can
//! sweep, and only then kills the process group. The helper's stderr is its
//! report of what it stopped.

use super::Call;
use super::{Ctx, Display, Group, Output, OutputKind, Tool, ToolError, ToolKind, def, obj};
use crate::jobs::ShellConfig;
use crate::permission::{Access, Outside, blocked_in, outside};
use crate::sandbox::report::{parse_report, swept_note};
use crate::sandbox::{GRACE, Policy, StopOnDrop};
use serde_json::{Value, json};
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::io::AsyncReadExt;

const DEFAULT_TIMEOUT_S: u64 = 120;
const MAX_TIMEOUT_S: u64 = 1800;

pub struct Shell {
    /// The longest a background command runs, in seconds.
    background_timeout: u64,
}

impl Shell {
    pub const NAME: &str = "shell";

    pub fn new(cfg: &ShellConfig) -> Self {
        Self { background_timeout: cfg.background_timeout.max(1) }
    }
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
            "Run a bash command and return its combined stdout/stderr. Use for builds, tests, git, and anything without a dedicated tool. Default timeout 120s. background = true returns a job id at once and you are told when it ends: use it to wait on CI or a long build, with a command that exits on what you wait for.",
            obj(
                json!({
                    "command": { "type": "string" },
                    "workdir": { "type": "string", "description": "Directory to run in; defaults to the session's" },
                    "timeout": { "type": "integer", "description": "Seconds, max 1800" },
                    "background": { "type": "boolean", "description": "Run as a background job" }
                }),
                &["command"],
            ),
        )
    }
    fn display(&self) -> Display {
        Display { primary_arg: "command", preview_lines: 4 }
    }
    async fn call(&self, ctx: &Ctx, a: Value) -> Result<Output, ToolError> {
        self.call_with(ctx, &Call::detached(""), a).await
    }
    async fn call_with(&self, ctx: &Ctx, call: &Call, a: Value) -> Result<Output, ToolError> {
        run(ctx, call, self.background_timeout, &a).await.map(|o| o.with_kind(OutputKind::Tail)).map_err(|mut e| {
            e.kind = OutputKind::Tail;
            e
        })
    }
}

/// Most times one call may be run again after the user widens the sandbox for
/// it: a read refused, then the write that follows it.
const MAX_ASKS: usize = 2;

/// Blocked shell calls in one turn from which every result tells the model to
/// stop searching and ask, no more prompts are shown, and a command that
/// names a path outside the sandbox is not run at all (#64): a weak model
/// reads past the words, but not past a refusal.
pub const STOP_AFTER: usize = 3;

async fn run(ctx: &Ctx, call: &Call, background_max: u64, a: &Value) -> Result<Output, ToolError> {
    let command = a.get("command").and_then(Value::as_str).ok_or("command is required")?;
    let timeout = a.get("timeout").and_then(Value::as_u64).unwrap_or(DEFAULT_TIMEOUT_S).clamp(1, MAX_TIMEOUT_S);
    let cwd =
        a.get("workdir").and_then(Value::as_str).map(|d| super::fs::resolve(ctx, d)).unwrap_or_else(|| ctx.cwd.clone());
    ctx.can_path(Shell::NAME, Access::Read, &cwd).await?;
    let mut policy = ctx.policy();
    let stopped = ctx.shell_blocks() >= STOP_AFTER;
    // A command that hides its errors (`2>/dev/null`, `|| echo missing`)
    // leaves nothing to ask about afterwards, so the paths it names are
    // asked about before it runs.
    let named = outside(&policy, command, &cwd);
    if stopped && !named.is_empty() {
        return Err(refused(ctx, &named));
    }
    let mut declined = vec![];
    if !named.ask.is_empty() && !stopped {
        match ctx.perms.shell(command, Access::Read, named.ask.clone(), true, ctx.cancel.clone()).await {
            Some(extra) => policy = policy.widened(&extra, &[]),
            None => declined = named.ask.clone(),
        }
        if *ctx.cancel.borrow() {
            return Err(ToolError::new("interrupted", "[interrupted before the command ran]"));
        }
    }
    if a.get("background").and_then(Value::as_bool).unwrap_or(false) {
        let job = super::shell_job::start(ctx, call, command, &cwd, &policy, background_max, a.get("timeout"))?;
        let (text, data) = sandbox_note(ctx, job.text, &named, &policy, false);
        return Ok(match data {
            Some(d) => Output::plain(text).with_data(json!({ "job": job.id, "sandbox": d["sandbox"] })),
            None => Output::plain(text).with_data(json!({ "job": job.id })),
        });
    }
    let mut asks = 0;
    let r = loop {
        let r = run_once(ctx, &policy, command, &cwd, timeout).await;
        // A Landlock denial is otherwise visible only in the command's own
        // output, so asking comes after the fact: the user sees which paths
        // it named and that approving runs the command again.
        let Err(e) = &r else { break r };
        if e.code != "exit" || asks == MAX_ASKS || stopped {
            break r;
        }
        let Some((access, paths)) = blocked_in(&policy, &e.message) else { break r };
        let paths: Vec<_> = paths.into_iter().filter(|p| !declined.contains(p)).collect();
        if paths.is_empty() {
            break r;
        }
        let Some(extra) = ctx.perms.shell(command, access, paths, false, ctx.cancel.clone()).await else { break r };
        policy = match access {
            Access::Read => policy.widened(&extra, &[]),
            Access::Write => policy.widened(&[], &extra),
        };
        asks += 1;
    };
    match r {
        Ok(out) => {
            let (text, data) = sandbox_note(ctx, out, &named, &policy, false);
            let o = Output::plain(text);
            Ok(match data {
                Some(d) => o.with_data(d),
                None => o,
            })
        }
        Err(mut e) => {
            let hinted = e.message.contains("[sandbox:");
            let (text, data) = sandbox_note(ctx, std::mem::take(&mut e.message), &named, &policy, hinted);
            e.message = text;
            e.data = data.or(e.data);
            Err(e)
        }
    }
}

/// The answer to a command that names paths outside the sandbox once the turn
/// has had `STOP_AFTER` blocked calls: it is not run.
fn refused(ctx: &Ctx, named: &Outside) -> ToolError {
    let n = ctx.shell_blocked();
    let places: Vec<String> = [&named.ask, &named.broad, &named.construct, &named.protected]
        .into_iter()
        .flatten()
        .map(|p| p.display().to_string())
        .collect();
    ToolError::new(
        "sandbox",
        format!(
            "[sandbox: not run. {n} shell calls this turn were blocked by the sandbox, and this one names {} outside it too. Stop searching outside the workspace and ask the user where the file is, or to allow the path.]",
            places.join(", ")
        ),
    )
    .with_data(json!({ "sandbox": { "outside": places, "blocked_this_turn": n, "stop": true, "refused": true } }))
}

/// `out` with what the sandbox kept from the command said, whatever its exit
/// code or redirects: the paths it named that are still out of reach, and,
/// from the `STOP_AFTER`th blocked call in a turn, a nudge to stop and ask.
/// `hinted`: the output already showed a denial. The data, for the journal
/// and scorers, says what was blocked and whether the stop was given.
fn sandbox_note(ctx: &Ctx, mut out: String, named: &Outside, policy: &Policy, hinted: bool) -> (String, Option<Value>) {
    let blocked = named.clone().still_blocked(policy);
    let note = blocked.note(&ctx.paths.home, &ctx.paths.skills_dir());
    if note.is_empty() && !hinted {
        return (out, None);
    }
    if !note.is_empty() {
        out.push('\n');
        out.push_str(&note);
    }
    let n = ctx.shell_blocked();
    if n >= STOP_AFTER {
        out.push_str(&format!(
            "\n[sandbox: {n} shell calls this turn were blocked by the sandbox. Stop searching outside the workspace and ask the user where the file is, or to allow the path.]"
        ));
    }
    let shown = |v: &[std::path::PathBuf]| v.iter().map(|p| p.display().to_string()).collect::<Vec<_>>();
    let data = json!({ "sandbox": {
        "outside": shown(&[blocked.ask, blocked.protected].concat()),
        "broad": shown(&blocked.broad),
        "construct": !blocked.construct.is_empty(),
        "from_output": hinted,
        "blocked_this_turn": n,
        "stop": n >= STOP_AFTER,
    }});
    (out, Some(data))
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
    let note = swept_note(swept);
    if !note.is_empty() {
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&note);
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
