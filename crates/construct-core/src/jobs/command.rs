//! The body of a background shell command: the same sandbox helper and policy
//! as a foreground call, but the helper lives as long as the job, not the
//! call. It ends the job when the command exits, the time limit passes or the
//! job is cancelled, and in each case the helper sweeps what the command left.

use super::{Body, Finish, Run, Status};
use crate::sandbox::{GRACE, Policy, StopOnDrop, report};
use std::path::PathBuf;
use std::process::Stdio;
use tokio::io::AsyncReadExt;

/// A body that runs `command` in `cwd` under `policy`.
pub fn body(policy: Policy, command: String, cwd: PathBuf) -> Body {
    Box::new(move |run| Box::pin(execute(policy, command, cwd, run)))
}

fn failed(note: String) -> Finish {
    Finish { status: Status::Failed, exit: None, note }
}

async fn execute(policy: Policy, command: String, cwd: PathBuf, mut run: Run) -> Finish {
    let shell = if std::path::Path::new("/bin/bash").exists() { "/bin/bash" } else { "/bin/sh" };
    let mut cmd = policy.command(shell, &format!("exec 2>&1\n{command}"));
    cmd.current_dir(&cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("CONSTRUCT", "1")
        .env("GIT_PAGER", "cat")
        .env("PAGER", "cat")
        .process_group(0);
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return failed(format!("[spawn failed: {e}]")),
    };
    let mut guard = StopOnDrop(child.id());
    let (Some(mut stdout), Some(mut stderr)) = (child.stdout.take(), child.stderr.take()) else {
        return failed("[spawn failed: no pipes]".into());
    };
    let deadline = tokio::time::sleep(run.timeout);
    tokio::pin!(deadline);
    let mut chunk = [0u8; 16384];
    let mut stopped: Option<Status> = None;
    let mut watching = true;
    loop {
        tokio::select! {
            n = stdout.read(&mut chunk) => match n {
                Ok(0) | Err(_) => break,
                Ok(n) => run.tail.push(&chunk[..n]),
            },
            _ = &mut deadline => { stopped = Some(Status::Timeout); break; }
            r = run.cancel.wait_for(|c| *c), if watching => {
                if r.is_ok() { stopped = Some(Status::Cancelled); break; }
                watching = false;
            }
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
    let mut said = Vec::new();
    let _ = tokio::time::timeout(GRACE, stderr.read_to_end(&mut said)).await;
    let (swept, helper) = report::parse_report(&String::from_utf8_lossy(&said));
    let mut note = report::swept_note(swept);
    if !helper.trim().is_empty() {
        note = format!("{}\n{note}", helper.trim());
    }
    let exit = status.and_then(|s| s.code());
    let status = match (stopped, exit) {
        (Some(why), _) => why,
        (None, Some(0)) => Status::Completed,
        (None, _) => Status::Failed,
    };
    Finish { status, exit, note: note.trim().to_string() }
}
