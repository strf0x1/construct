//! Concrete developer operations approved before using host authentication.
//! Arguments and input are prepared once; approval never grants a shell escape.

mod git;
mod github;
mod prepare;
mod runner;
#[cfg(test)]
mod tests;

use super::{Call, Ctx, Display, Group, Output, Tool, ToolError, ToolKind, def};
use serde_json::{Value, json};

pub struct Developer;

impl Developer {
    pub const NAME: &str = "developer";
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
        def(
            Self::NAME,
            "Request approval for a concrete developer operation using the host's authentication. Use instead of probing credentials with shell. Stage files with shell before commit; commits are unsigned and skip hooks. Push sends the captured branch commit to the named remote without force or hooks. PR creation never pushes. Each operation is approved separately. After an unknown remote outcome, reconcile with a read-only query before proceeding; never repeat a write automatically.",
            json!({"type":"object", "properties": {
                "operation":{"type":"string", "enum":["issue_create","commit","push","pr_create"]},
                "repo":{"type":"string", "description":"issue_create/pr_create: required explicit OWNER/REPO on github.com"},
                "title":{"type":"string", "description":"issue_create/pr_create: required title"},
                "body":{"type":"string", "description":"issue_create/pr_create: required complete body, may be empty"},
                "base":{"type":"string", "description":"pr_create: required base branch"},
                "head":{"type":"string", "description":"pr_create: required existing head branch, optionally OWNER:BRANCH"},
                "workdir":{"type":"string", "description":"commit/push: repository directory, defaults to session working directory"},
                "message":{"type":"string", "description":"commit: required complete commit message"},
                "remote":{"type":"string", "description":"push: required configured SSH/HTTPS remote name"},
                "branch":{"type":"string", "description":"push: required source and destination branch name"}
            }, "required":["operation"], "additionalProperties":false}),
        )
    }
    async fn call(&self, ctx: &Ctx, args: Value) -> Result<Output, ToolError> {
        self.call_with(ctx, &Call::detached(""), args).await
    }
    async fn call_with(&self, ctx: &Ctx, _call: &Call, args: Value) -> Result<Output, ToolError> {
        let prepared = prepare::prepare(ctx, args).await?;
        if !ctx.perms.operation(prepared.request.clone(), ctx.cancel.clone()).await {
            return Err(ToolError::new(
                "denied",
                "Developer operation was not approved. The requested action was not executed.",
            ));
        }
        prepared.execute(ctx).await
    }
}
