//! A file tool's check against the sandbox, with the question that goes with
//! it. Tools call `can_read` / `can_write` with the argument that names the
//! path, before doing any work; a path the sandbox allows costs nothing, and
//! one it doesn't becomes a prompt (`permission`) in a session that has one.

use super::{Ctx, ToolError};
use crate::permission::Access;
use crate::sandbox::Policy;
use serde_json::Value;

impl Ctx {
    /// The session's sandbox policy: the base, plus what the user approved
    /// for the rest of the session. What a shell command runs under.
    pub fn policy(&self) -> Policy {
        self.perms.policy()
    }

    pub async fn can_read(&self, tool: &str, args: &Value, key: &str) -> Result<Policy, ToolError> {
        self.can(tool, Access::Read, args, key).await
    }

    pub async fn can_write(&self, tool: &str, args: &Value, key: &str) -> Result<Policy, ToolError> {
        self.can(tool, Access::Write, args, key).await
    }

    /// The path in `args[key]`. A call with none (or an empty one) is the
    /// tool's own error to report, or defaults to the working directory.
    async fn can(&self, tool: &str, access: Access, args: &Value, key: &str) -> Result<Policy, ToolError> {
        let Some(raw) = super::arg(args, key).filter(|p| !p.trim().is_empty()) else { return Ok(self.policy()) };
        let path = super::fs::resolve(self, raw);
        self.can_path(tool, access, &path).await
    }

    /// The policy the tool opens `path` under: the session's, plus the path
    /// itself when only this call was approved (a "once" is not remembered,
    /// so the next look at `policy()` would refuse it).
    pub async fn can_path(&self, tool: &str, access: Access, path: &std::path::Path) -> Result<Policy, ToolError> {
        let asked = self.perms.path_asked(tool, access, path, self.cancel.clone()).await.map_err(ToolError::sandbox)?;
        let policy = self.policy();
        Ok(match (asked, access) {
            (None, _) => policy,
            (Some(real), Access::Read) => policy.widened(&[real], &[]),
            (Some(real), Access::Write) => policy.widened(&[], &[real]),
        })
    }
}
