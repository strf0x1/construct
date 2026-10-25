//! write: create a file or replace its content; an existing file must have
//! been read first.

use super::fs::resolve;
use super::{Ctx, Display, Group, Output, Tool, ToolError, ToolKind, arg, def, obj};
use crate::sandbox::{Policy, open};
use serde_json::{Value, json};

pub struct Write;

impl Write {
    pub const NAME: &str = "write";
}

#[async_trait::async_trait]
impl Tool for Write {
    fn name(&self) -> &'static str {
        Self::NAME
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Edit
    }
    fn group(&self) -> Group {
        Group::Core
    }
    fn definition(&self) -> Value {
        def(
            Self::NAME,
            "Create a file or replace its whole content. An existing file must be read first. Prefer edit for changes.",
            obj(json!({ "path": { "type": "string" }, "content": { "type": "string" } }), &["path", "content"]),
        )
    }
    fn display(&self) -> Display {
        Display { primary_arg: "path", preview_lines: 8 }
    }
    async fn call(&self, ctx: &Ctx, a: Value) -> Result<Output, ToolError> {
        let policy = ctx.can_write(Self::NAME, &a, "path").await?;
        run(ctx, &policy, &a).map(Output::plain)
    }
}

fn run(ctx: &Ctx, policy: &Policy, a: &Value) -> Result<String, ToolError> {
    let path = resolve(ctx, arg(a, "path").ok_or("path is required")?);
    let content = arg(a, "content").ok_or("content is required")?;
    let existed = open::exists(policy, &path);
    if existed && !ctx.was_read(&path) {
        return Err(ToolError::new(
            "not_read",
            format!("{} exists and hasn't been read this session; read it first so nothing is lost", path.display()),
        ));
    }
    open::write_file(policy, &path, content.as_bytes())?;
    ctx.mark_read(&path);
    Ok(format!(
        "{} {} ({} lines)",
        if existed { "overwrote" } else { "created" },
        path.display(),
        content.lines().count()
    ))
}
