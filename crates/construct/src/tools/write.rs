//! write: create a file or replace its content; an existing file must have
//! been read first.

use super::fs::resolve;
use super::{Ctx, Display, Group, Output, Tool, ToolError, arg, def, obj};
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
        run(ctx, &a).map(Output::plain)
    }
}

fn run(ctx: &Ctx, a: &Value) -> Result<String, ToolError> {
    let path = resolve(ctx, arg(a, "path").ok_or("path is required")?);
    ctx.sandbox.check_write(&path).map_err(ToolError::sandbox)?;
    let content = arg(a, "content").ok_or("content is required")?;
    let existed = path.exists();
    if existed && !ctx.was_read(&path) {
        return Err(ToolError::new(
            "not_read",
            format!("{} exists and hasn't been read this session; read it first so nothing is lost", path.display()),
        ));
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(&path, content).map_err(|e| format!("{}: {e}", path.display()))?;
    ctx.mark_read(&path);
    Ok(format!(
        "{} {} ({} lines)",
        if existed { "overwrote" } else { "created" },
        path.display(),
        content.lines().count()
    ))
}
