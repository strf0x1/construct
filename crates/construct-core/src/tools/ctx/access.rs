//! Sandbox and permission checks on `Ctx`. Tools ask before reading or writing
//! paths, running commands that name paths outside the sandbox, or executing
//! host-authenticated operations; `Ctx` pairs each check with its cancellation
//! watch and `Perms` handle so callers never wire those together by hand.

use super::Ctx;
use crate::permission::{Access, OperationAsk, Outside};
use crate::sandbox::Policy;
use crate::tools::{ToolError, arg};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// Blocked shell calls in one turn from which every result tells the model to
/// stop searching and ask, no more prompts are shown, and a command that
/// names a path outside the sandbox is not run at all (#64): a weak model
/// reads past the words, but not past a refusal.
pub(crate) const SHELL_STOP_AFTER: usize = 3;

impl Ctx {
    /// The session's sandbox policy: the base, plus what the user approved
    /// for the rest of the session. What a shell command runs under.
    pub fn policy(&self) -> Policy {
        self.perms.policy()
    }

    pub(in crate::tools) async fn can_read(&self, tool: &str, args: &Value, key: &str) -> Result<Policy, ToolError> {
        self.can(tool, Access::Read, args, key).await
    }

    pub(in crate::tools) async fn can_write(&self, tool: &str, args: &Value, key: &str) -> Result<Policy, ToolError> {
        self.can(tool, Access::Write, args, key).await
    }

    /// The path in `args[key]`. A call with none (or an empty one) is the
    /// tool's own error to report, or defaults to the working directory.
    async fn can(&self, tool: &str, access: Access, args: &Value, key: &str) -> Result<Policy, ToolError> {
        let Some(raw) = arg(args, key).filter(|p| !p.trim().is_empty()) else { return Ok(self.policy()) };
        let path = self.resolve_path(raw);
        self.can_path(tool, access, &path).await
    }

    /// The policy the tool opens `path` under: the session's, plus the path
    /// itself when only this call was approved (a "once" is not remembered,
    /// so the next look at `policy()` would refuse it).
    pub(in crate::tools) async fn can_path(
        &self,
        tool: &str,
        access: Access,
        path: &Path,
    ) -> Result<Policy, ToolError> {
        let asked = self.perms.path_asked(tool, access, path, self.cancel.clone()).await.map_err(ToolError::sandbox)?;
        let policy = self.policy();
        Ok(match (asked, access) {
            (None, _) => policy,
            (Some(real), Access::Read) => policy.widened(&[real], &[]),
            (Some(real), Access::Write) => policy.widened(&[], &[real]),
        })
    }

    /// Ask the user whether a shell command may access `paths`.
    pub(in crate::tools) async fn ask_shell(
        &self,
        command: &str,
        access: Access,
        paths: Vec<PathBuf>,
        ahead: bool,
    ) -> Option<Vec<PathBuf>> {
        self.perms.shell(command, access, paths, ahead, self.cancel.clone()).await
    }

    /// Ask the user to approve a concrete host-authenticated operation.
    pub async fn approve_operation(&self, request: OperationAsk) -> bool {
        self.perms.operation(request, self.cancel.clone()).await
    }

    /// Check whether this turn's blocked-shell budget is exhausted: returns
    /// `Err` refusing a command that names outside paths once `SHELL_STOP_AFTER`
    /// calls have been blocked, or `Ok(stopped)` otherwise.
    pub(in crate::tools) fn shell_guard(&self, named: &Outside) -> Result<bool, ToolError> {
        let stopped = self.shell_blocks() >= SHELL_STOP_AFTER;
        if stopped && !named.is_empty() {
            return Err(self.refuse_shell(named));
        }
        Ok(stopped)
    }

    fn refuse_shell(&self, named: &Outside) -> ToolError {
        let n = self.shell_blocked();
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

    /// Annotate a shell command's output with what the sandbox kept from it:
    /// the named paths still out of reach, and, from the `SHELL_STOP_AFTER`th
    /// blocked call in a turn, a nudge to stop and ask.
    pub(in crate::tools) fn annotate_shell_sandbox(
        &self,
        mut out: String,
        named: &Outside,
        policy: &Policy,
        hinted: bool,
    ) -> (String, Option<Value>) {
        let blocked = named.clone().still_blocked(policy);
        let note = self.sandbox_block_note(&blocked);
        if note.is_empty() && !hinted {
            return (out, None);
        }
        if !note.is_empty() {
            out.push('\n');
            out.push_str(&note);
        }
        let n = self.shell_blocked();
        if n >= SHELL_STOP_AFTER {
            out.push_str(&format!(
                "\n[sandbox: {n} shell calls this turn were blocked by the sandbox. Stop searching outside the workspace and ask the user where the file is, or to allow the path.]"
            ));
        }
        let shown = |v: &[PathBuf]| v.iter().map(|p| p.display().to_string()).collect::<Vec<_>>();
        let data = json!({ "sandbox": {
            "outside": shown(&[blocked.ask, blocked.protected].concat()),
            "broad": shown(&blocked.broad),
            "construct": !blocked.construct.is_empty(),
            "from_output": hinted,
            "blocked_this_turn": n,
            "stop": n >= SHELL_STOP_AFTER,
        }});
        (out, Some(data))
    }

    fn sandbox_block_note(&self, blocked: &Outside) -> String {
        blocked.note(&self.paths.home, &self.paths.skills_dir(), self.perms.tools())
    }
}
