//! glob: find files by pattern, newest first, honouring .gitignore.

use super::fs::{display, resolve, walker};
use super::{Ctx, Display, Group, Output, Tool, ToolError, arg, blocking, def, obj};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::time::SystemTime;

const GLOB_MAX: usize = 200;

pub struct Glob;

impl Glob {
    pub const NAME: &str = "glob";
}

#[async_trait::async_trait]
impl Tool for Glob {
    fn name(&self) -> &'static str {
        Self::NAME
    }
    fn group(&self) -> Group {
        Group::Core
    }
    fn definition(&self) -> Value {
        def(
            Self::NAME,
            "Find files by glob pattern (e.g. src/**/*.rs), newest first. Respects .gitignore.",
            obj(json!({ "pattern": { "type": "string" }, "path": { "type": "string" } }), &["pattern"]),
        )
    }
    fn readonly(&self) -> bool {
        true
    }
    fn display(&self) -> Display {
        Display { primary_arg: "pattern", ..Display::default() }
    }
    async fn call(&self, ctx: &Ctx, a: Value) -> Result<Output, ToolError> {
        blocking(ctx, || run(ctx, &a))
    }
}

fn run(ctx: &Ctx, a: &Value) -> Result<String, ToolError> {
    let pattern = arg(a, "pattern").ok_or("pattern is required")?;
    let root = arg(a, "path").map(|p| resolve(ctx, p)).unwrap_or_else(|| ctx.cwd.clone());
    ctx.sandbox.check_read(&root).map_err(ToolError::sandbox)?;
    let pat =
        if pattern.contains('/') || pattern.starts_with("**") { pattern.to_string() } else { format!("**/{pattern}") };
    let set =
        globset::GlobBuilder::new(&pat).literal_separator(true).build().map_err(|e| e.to_string())?.compile_matcher();
    let mut hits: Vec<(SystemTime, PathBuf)> = Vec::new();
    for e in walker(&root, pattern.starts_with('.')).build().flatten() {
        if !e.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let rel = e.path().strip_prefix(&root).unwrap_or(e.path());
        if set.is_match(rel) {
            let m = e.metadata().ok().and_then(|m| m.modified().ok()).unwrap_or(SystemTime::UNIX_EPOCH);
            hits.push((m, e.path().to_path_buf()));
        }
    }
    hits.sort_by_key(|h| std::cmp::Reverse(h.0));
    let n = hits.len();
    let mut out: Vec<String> = hits.into_iter().take(GLOB_MAX).map(|(_, p)| display(ctx, &p)).collect();
    if n == 0 {
        return Ok("no files matched".into());
    }
    if n > GLOB_MAX {
        out.push(format!("… {n} matches, showing the {GLOB_MAX} most recently modified"));
    }
    Ok(out.join("\n"))
}
