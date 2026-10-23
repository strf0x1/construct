//! read: a text file with line numbers, or a directory listing.

use super::fs::{encoding_error, is_utf16, resolve};
use super::{Ctx, Display, Group, Output, Tool, ToolError, ToolKind, arg, blocking, def, num, obj};
use serde_json::{Value, json};
use std::path::Path;

pub(super) const READ_LINES: usize = 2000;
pub(super) const LINE_MAX: usize = 2000;

pub struct Read;

impl Read {
    pub const NAME: &str = "read";
}

#[async_trait::async_trait]
impl Tool for Read {
    fn name(&self) -> &'static str {
        Self::NAME
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Read
    }
    fn group(&self) -> Group {
        Group::Core
    }
    fn definition(&self) -> Value {
        def(
            Self::NAME,
            "Read a text file with line numbers (or list a directory). Reads up to 2000 lines; use offset/limit for more.",
            obj(
                json!({
                    "path": { "type": "string" },
                    "offset": { "type": "integer", "description": "1-based first line" },
                    "limit": { "type": "integer" }
                }),
                &["path"],
            ),
        )
    }
    fn readonly(&self) -> bool {
        true
    }
    fn display(&self) -> Display {
        Display { primary_arg: "path", preview_lines: 0 }
    }
    async fn call(&self, ctx: &Ctx, a: Value) -> Result<Output, ToolError> {
        ctx.can_read(Self::NAME, &a, "path").await?;
        blocking(ctx, || run(ctx, &a))
    }
}

fn run(ctx: &Ctx, a: &Value) -> Result<String, ToolError> {
    let path = resolve(ctx, arg(a, "path").ok_or("path is required")?);
    let meta = std::fs::metadata(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    if meta.is_dir() {
        return list_dir(&path);
    }
    let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    if is_utf16(&bytes) {
        return Err(encoding_error(&path.display().to_string(), true));
    }
    if bytes.iter().take(8192).any(|&b| b == 0) {
        return Err(format!("{} looks binary ({} bytes); not shown", path.display(), bytes.len()).into());
    }
    let text = String::from_utf8_lossy(&bytes);
    let offset = num(a, "offset").unwrap_or(1).max(1);
    let limit = num(a, "limit").unwrap_or(READ_LINES);
    let total = text.lines().count();
    let mut out = String::with_capacity(bytes.len().min(256 * 1024));
    for (i, line) in text.lines().enumerate().skip(offset - 1).take(limit) {
        let line = line.strip_suffix('\r').unwrap_or(line);
        let line = if line.len() > LINE_MAX { crate::llm::truncate(line, LINE_MAX) } else { line.to_string() };
        out.push_str(&format!("{:>5}\t{line}\n", i + 1));
    }
    let shown_end = (offset - 1 + limit).min(total);
    if total == 0 {
        out.push_str("(empty file)");
    } else if shown_end < total {
        out.push_str(&format!("… {total} lines total; showing {offset}-{shown_end}. Use offset to read more.\n"));
    }
    ctx.mark_read(&path);
    Ok(out)
}

pub(super) fn list_dir(path: &Path) -> Result<String, ToolError> {
    let mut names: Vec<String> = std::fs::read_dir(path)
        .map_err(|e| e.to_string())?
        .flatten()
        .map(|e| {
            let n = e.file_name().to_string_lossy().to_string();
            if e.path().is_dir() { format!("{n}/") } else { n }
        })
        .collect();
    names.sort();
    Ok(format!("{} is a directory:\n{}", path.display(), names.join("\n")))
}
