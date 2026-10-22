//! grep: regex search with ripgrep's own crates, so it honours .gitignore
//! and needs nothing installed.

use super::fs::{display, resolve, walker};
use super::{Ctx, Display, Group, Output, Tool, ToolError, ToolKind, arg, blocking, def, obj};
use grep_regex::RegexMatcherBuilder;
use grep_searcher::{BinaryDetection, SearcherBuilder, sinks::UTF8};
use serde_json::{Value, json};
use std::path::Path;

const GREP_MAX: usize = 200;

pub struct Grep;

impl Grep {
    pub const NAME: &str = "grep";
}

#[async_trait::async_trait]
impl Tool for Grep {
    fn name(&self) -> &'static str {
        Self::NAME
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Search
    }
    fn group(&self) -> Group {
        Group::Core
    }
    fn definition(&self) -> Value {
        def(
            Self::NAME,
            "Search file contents with a regex (ripgrep engine, respects .gitignore). Returns path:line: text.",
            obj(
                json!({
                    "pattern": { "type": "string" },
                    "path": { "type": "string", "description": "File or directory; defaults to the session's" },
                    "include": { "type": "string", "description": "Glob filter, e.g. *.rs" },
                    "ignore_case": { "type": "boolean" }
                }),
                &["pattern"],
            ),
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
    let insensitive = a.get("ignore_case").and_then(Value::as_bool).unwrap_or(false);
    let matcher = RegexMatcherBuilder::new()
        .case_insensitive(insensitive)
        .build(pattern)
        .map_err(|e| format!("bad regex: {e}"))?;
    let include = match arg(a, "include") {
        Some(g) => {
            let g = if g.contains('/') { g.to_string() } else { format!("**/{g}") };
            Some(globset::Glob::new(&g).map_err(|e| e.to_string())?.compile_matcher())
        }
        None => None,
    };
    let mut searcher = SearcherBuilder::new().binary_detection(BinaryDetection::quit(0)).line_number(true).build();
    let mut out: Vec<String> = Vec::new();
    let mut total = 0usize;
    let mut files = 0usize;
    let mut search_file = |path: &Path, out: &mut Vec<String>, total: &mut usize| {
        let mut found = false;
        let shown = display(ctx, path);
        let _ = searcher.search_path(
            &matcher,
            path,
            UTF8(|ln, line| {
                *total += 1;
                found = true;
                if out.len() < GREP_MAX {
                    out.push(format!("{shown}:{ln}: {}", crate::llm::truncate(line.trim_end(), 300)));
                }
                Ok(true)
            }),
        );
        found
    };
    if root.is_file() {
        if search_file(&root, &mut out, &mut total) {
            files += 1;
        }
    } else {
        for e in walker(&root, false).build().flatten() {
            if !e.file_type().is_some_and(|t| t.is_file()) {
                continue;
            }
            if let Some(inc) = &include {
                let rel = e.path().strip_prefix(&root).unwrap_or(e.path());
                if !inc.is_match(rel) {
                    continue;
                }
            }
            if search_file(e.path(), &mut out, &mut total) {
                files += 1;
            }
        }
    }
    if total == 0 {
        return Ok("no matches".into());
    }
    if total > out.len() {
        out.push(format!("… {total} matches in {files} files, showing {}. Narrow the pattern or path.", out.len()));
    }
    Ok(out.join("\n"))
}
