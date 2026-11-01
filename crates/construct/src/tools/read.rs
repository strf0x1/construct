//! read: a text file with line numbers, or a directory listing.

use super::fs::{encoding_error, is_utf16, resolve};
use super::{Ctx, Display, Group, Output, Tool, ToolError, ToolKind, arg, blocking, def, num, obj};
use crate::sandbox::{Policy, open};
use serde_json::{Value, json};
use std::path::Path;

pub(super) const READ_LINES: usize = 2000;
pub(super) const LINE_MAX: usize = 2000;
/// Room kept under the result cap for read's closing line.
const FOOTER: usize = 120;

/// Rows from line `offset` on, up to the result cap: a file bigger than a
/// small window pages (offset) instead of losing its middle to `cap_output`.
/// The first row always goes in. Returns the page and its last line number.
pub(super) fn page(ctx: &Ctx, offset: usize, rows: impl Iterator<Item = String>) -> (String, usize) {
    let (mut out, mut end) = (String::new(), offset - 1);
    for row in rows {
        if !out.is_empty() && out.len() + row.len() + FOOTER > ctx.output_max() {
            break;
        }
        out.push_str(&row);
        end += 1;
    }
    (out, end)
}

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
        let policy = ctx.can_read(Self::NAME, &a, "path").await?;
        blocking(ctx, || run(ctx, &policy, &a))
    }
}

fn run(ctx: &Ctx, policy: &Policy, a: &Value) -> Result<String, ToolError> {
    let path = resolve(ctx, arg(a, "path").ok_or("path is required")?);
    let meta = open::metadata(policy, &path)?;
    if meta.is_dir() {
        return list_dir(policy, &path);
    }
    let bytes = open::read_file(policy, &path)?;
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
    let rows = text.lines().enumerate().skip(offset - 1).take(limit).map(|(i, line)| {
        let line = line.strip_suffix('\r').unwrap_or(line);
        let line = if line.len() > LINE_MAX { crate::llm::truncate(line, LINE_MAX) } else { line.to_string() };
        format!("{:>5}\t{line}\n", i + 1)
    });
    let (mut out, shown_end) = page(ctx, offset, rows);
    if total == 0 {
        out.push_str("(empty file)");
    } else if shown_end < total {
        out.push_str(&format!("… {total} lines total; showing {offset}-{shown_end}. Use offset to read more.\n"));
    }
    ctx.mark_read(&path);
    Ok(out)
}

pub(super) fn list_dir(policy: &Policy, path: &Path) -> Result<String, ToolError> {
    let mut names = open::list_dir(policy, path)?;
    names.sort();
    Ok(format!("{} is a directory:\n{}", path.display(), names.join("\n")))
}
