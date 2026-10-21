//! read, write, grep and glob, in-process. grep and glob use ripgrep's own
//! crates, so they honour .gitignore and need nothing installed.

use super::Ctx;
use grep_regex::RegexMatcherBuilder;
use grep_searcher::{BinaryDetection, SearcherBuilder, sinks::UTF8};
use ignore::WalkBuilder;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

const READ_LINES: usize = 2000;
const LINE_MAX: usize = 2000;
const GREP_MAX: usize = 200;
const GLOB_MAX: usize = 200;

pub fn resolve(ctx: &Ctx, p: &str) -> PathBuf {
    let p = p.trim();
    let expanded = match p.strip_prefix("~/") {
        Some(rest) => std::env::var_os("HOME").map(|h| PathBuf::from(h).join(rest)).unwrap_or_else(|| PathBuf::from(p)),
        None => PathBuf::from(p),
    };
    if expanded.is_absolute() { expanded } else { ctx.cwd.join(expanded) }
}

fn arg<'a>(a: &'a Value, k: &str) -> Option<&'a str> {
    a.get(k).and_then(Value::as_str)
}

fn num(a: &Value, k: &str) -> Option<usize> {
    a.get(k).and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse().ok()))).map(|n| n as usize)
}

pub fn read(ctx: &Ctx, a: &Value) -> Result<String, String> {
    let path = resolve(ctx, arg(a, "path").ok_or("path is required")?);
    ctx.sandbox.check_read(&path)?;
    let meta = std::fs::metadata(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    if meta.is_dir() {
        let mut names: Vec<String> = std::fs::read_dir(&path)
            .map_err(|e| e.to_string())?
            .flatten()
            .map(|e| {
                let n = e.file_name().to_string_lossy().to_string();
                if e.path().is_dir() { format!("{n}/") } else { n }
            })
            .collect();
        names.sort();
        return Ok(format!("{} is a directory:\n{}", path.display(), names.join("\n")));
    }
    let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    if bytes.iter().take(8192).any(|&b| b == 0) {
        return Err(format!("{} looks binary ({} bytes); not shown", path.display(), bytes.len()));
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

pub fn write(ctx: &Ctx, a: &Value) -> Result<String, String> {
    let path = resolve(ctx, arg(a, "path").ok_or("path is required")?);
    ctx.sandbox.check_write(&path)?;
    let content = arg(a, "content").ok_or("content is required")?;
    let existed = path.exists();
    if existed && !ctx.was_read(&path) {
        return Err(format!(
            "{} exists and hasn't been read this session; read it first so nothing is lost",
            path.display()
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

pub fn edit(ctx: &Ctx, a: &Value) -> Result<String, String> {
    let path = resolve(ctx, arg(a, "path").ok_or("path is required")?);
    ctx.sandbox.check_write(&path)?;
    let old = arg(a, "old_string").ok_or("old_string is required")?;
    let new = arg(a, "new_string").ok_or("new_string is required")?;
    let all = a.get("replace_all").and_then(Value::as_bool).unwrap_or(false);
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let updated = super::edit::replace_once(&text, old, new, all)?;
    std::fs::write(&path, &updated).map_err(|e| e.to_string())?;
    ctx.mark_read(&path);
    Ok(format!("edited {}\n{}", path.display(), diff_summary(&text, &updated)))
}

/// A compact unified diff so the model (and the UI) can see what changed.
pub fn diff_summary(before: &str, after: &str) -> String {
    let diff = similar::TextDiff::from_lines(before, after);
    let mut out = String::new();
    for group in diff.grouped_ops(1).iter().take(6) {
        for op in group {
            for change in diff.iter_changes(op) {
                let sign = match change.tag() {
                    similar::ChangeTag::Delete => '-',
                    similar::ChangeTag::Insert => '+',
                    similar::ChangeTag::Equal => ' ',
                };
                let line = change.value().trim_end_matches('\n');
                out.push_str(&format!("{sign}{}\n", crate::llm::truncate(line, 240)));
            }
        }
        out.push_str("…\n");
    }
    crate::llm::truncate(&out, 4000)
}

fn walker(root: &Path, hidden: bool) -> WalkBuilder {
    let mut w = WalkBuilder::new(root);
    w.hidden(!hidden).git_ignore(true).git_global(true).git_exclude(true).parents(true).require_git(false);
    w.filter_entry(|e| e.file_name() != ".git");
    w
}

pub fn glob(ctx: &Ctx, a: &Value) -> Result<String, String> {
    let pattern = arg(a, "pattern").ok_or("pattern is required")?;
    let root = arg(a, "path").map(|p| resolve(ctx, p)).unwrap_or_else(|| ctx.cwd.clone());
    ctx.sandbox.check_read(&root)?;
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
    hits.sort_by(|a, b| b.0.cmp(&a.0));
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

pub fn grep(ctx: &Ctx, a: &Value) -> Result<String, String> {
    let pattern = arg(a, "pattern").ok_or("pattern is required")?;
    let root = arg(a, "path").map(|p| resolve(ctx, p)).unwrap_or_else(|| ctx.cwd.clone());
    ctx.sandbox.check_read(&root)?;
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

fn display(ctx: &Ctx, p: &Path) -> String {
    p.strip_prefix(&ctx.cwd).map(|r| r.display().to_string()).unwrap_or_else(|_| p.display().to_string())
}
