//! Starting a shell command as a background job: the call returns a handle at
//! once, and the registry (`jobs`) owns the command from there. The command
//! runs under the same sandbox policy the foreground call would, with the
//! permission questions already settled at spawn by `shell::run`.

use super::{Call, Ctx, ToolError};
use crate::jobs::{self, Kind};
use crate::sandbox::Policy;
use serde_json::Value;
use std::path::Path;
use std::time::Duration;

/// The handle a started job gives back.
pub(super) struct Started {
    pub id: String,
    pub text: String,
}

pub(super) fn start(
    ctx: &Ctx,
    call: &Call,
    command: &str,
    cwd: &Path,
    policy: &Policy,
    max_secs: u64,
    asked: Option<&Value>,
) -> Result<Started, ToolError> {
    if crate::child::depth() > 0 {
        return Err(ToolError::new("depth", "background jobs are not available inside a delegated agent"));
    }
    let Some(sink) = call.sink() else {
        return Err(ToolError::new("no_session", "background jobs need a session to report to"));
    };
    // The call can ask for less than the limit, never more.
    let secs = asked.and_then(Value::as_u64).map_or(max_secs, |s| s.clamp(1, max_secs));
    let id = ctx
        .jobs
        .start(jobs::Spec {
            kind: Kind::Shell,
            label: command.to_string(),
            call_id: call.id.clone(),
            timeout: Duration::from_secs(secs),
            sink,
            body: jobs::command_body(policy.clone(), command.to_string(), cwd.to_path_buf()),
        })
        .map_err(|e| ToolError::new("job_limit", e))?;
    let text = format!(
        "started job {id} in the background (time limit {secs}s). You will be told when it ends (completed, failed or timed out), with the end of its output, so don't poll it. The job tool reads it sooner or cancels it."
    );
    Ok(Started { id, text })
}
