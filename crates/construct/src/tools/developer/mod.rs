//! Concrete developer operations approved before using host authentication.
//! Arguments and input are prepared once; approval never grants a shell escape.
//! In a Deck task with a repository the GitHub operations go to Deck's
//! gateway instead (`gateway`), and host authentication is never used.

mod gateway;
mod git;
mod github;
mod prepare;
mod runner;
#[cfg(test)]
mod tests;

use super::{Call, Ctx, Display, Group, Output, Tool, ToolError, ToolKind, def};
use serde_json::{Value, json};

/// `gateway`: whether this process has Deck's GitHub gateway, which changes
/// the operations and the description the model sees.
pub struct Developer {
    gateway: bool,
}

impl Developer {
    pub const NAME: &str = "developer";

    pub fn new(gateway: bool) -> Self {
        Self { gateway }
    }
}

#[async_trait::async_trait]
impl Tool for Developer {
    fn name(&self) -> &'static str {
        Self::NAME
    }
    fn group(&self) -> Group {
        Group::Developer
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Execute
    }
    fn display(&self) -> Display {
        Display { primary_arg: "operation", preview_lines: 3 }
    }
    fn definition(&self) -> Value {
        if self.gateway {
            return def(
                Self::NAME,
                &gateway::DESCRIPTION.replace("{shell}", super::shell::Shell::NAME),
                json!({"type":"object", "properties": {
                    "operation":{"type":"string", "enum": gateway::OPERATIONS},
                    "workdir":{"type":"string", "description":"commit/push: repository directory, defaults to session working directory"},
                    "message":{"type":"string", "description":"commit: required complete commit message"},
                    "branch":{"type":"string", "description":"push: branch to publish; defaults to this task's branch, the only one usually writable"},
                    "title":{"type":"string", "description":"pr_create: required; pr_update: new title"},
                    "body":{"type":"string", "description":"pr_create/pr_update: complete body; an empty string clears it on update"},
                    "base":{"type":"string", "description":"pr_create/pr_update: defaults to the project's base"},
                    "head":{"type":"string", "description":"pr_create: defaults to this task's branch"},
                    "draft":{"type":"boolean", "description":"pr_create: default true; pr_update: false marks it ready for review"},
                    "number":{"type":"integer", "description":"pr_update/pr_review: the pull request number"}
                }, "required":["operation"], "additionalProperties":false}),
            );
        }
        def(
            Self::NAME,
            &format!(
                "Approve concrete commits, pushes and GitHub writes. In a linked Git worktree use status, stage and merge_start for its own checkout. Stage exact file paths before commit. Start a merge with one full commit ID, then resolve conflicts, stage and commit. In a regular checkout stage with {sh}. Commits are unsigned and skip hooks. Push sends the captured commit to the named remote without force or hooks. PR creation never pushes. After an unknown remote outcome, reconcile with a read-only query before repeating a write.",
                sh = super::shell::Shell::NAME
            ),
            json!({"type":"object", "properties": {
                "operation":{"type":"string", "enum":["issue_create","commit","push","pr_create",git::worktree::STATUS,git::worktree::STAGE,git::worktree::MERGE_START]},
                "repo":{"type":"string", "description":"issue_create/pr_create: required explicit OWNER/REPO on github.com"},
                "title":{"type":"string", "description":"issue_create/pr_create: required title"},
                "body":{"type":"string", "description":"issue_create/pr_create: required complete body, may be empty"},
                "base":{"type":"string", "description":"pr_create: required base branch"},
                "head":{"type":"string", "description":"pr_create: required existing head branch, optionally OWNER:BRANCH"},
                "workdir":{"type":"string", "description":"commit/push: repository directory, defaults to session working directory"},
                "message":{"type":"string", "description":"commit: required complete commit message"},
                "remote":{"type":"string", "description":"push: required configured SSH/HTTPS remote name"},
                "branch":{"type":"string", "description":"push: required source and destination branch name"},
                "paths":{"type":"array","items":{"type":"string"},"description":"stage: exact relative file paths in this linked worktree"},
                "commit":{"type":"string","description":"merge_start: one full Git commit ID"}
            }, "required":["operation"], "additionalProperties":false}),
        )
    }
    async fn call(&self, ctx: &Ctx, args: Value) -> Result<Output, ToolError> {
        self.call_with(ctx, &Call::detached(""), args).await
    }
    async fn call_with(&self, ctx: &Ctx, _call: &Call, args: Value) -> Result<Output, ToolError> {
        let operation = args.get("operation").and_then(Value::as_str);
        if let Some(url) = &ctx.github
            && operation != Some("commit")
        {
            return gateway::call(ctx, url, args).await;
        }
        if operation.is_some_and(git::worktree::is_operation) {
            return git::worktree::call(ctx, args).await;
        }
        let prepared = prepare::prepare(ctx, args).await?;
        // A local commit in a Deck task asks nobody: the VM is the sandbox and
        // the gateway decides what leaves it (Deck ADR 0008 §5).
        let deck_mode = ctx.github.is_some() && crate::secrets::managed();
        if !deck_mode && !ctx.perms.operation(prepared.request.clone(), ctx.cancel.clone()).await {
            return Err(ToolError::new(
                "denied",
                "Developer operation was not approved. The requested action was not executed.",
            ));
        }
        prepared.execute(ctx).await
    }
}
