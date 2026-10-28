//! grep: regex search with ripgrep's own crates, so it honours .gitignore
//! and needs nothing installed. The walk and the search run on every core;
//! the output is in path order whichever thread finishes first, and only the
//! first `GREP_MAX` lines are ever held, however many files match.

use super::fs::{display, resolve, walker};
use super::{Ctx, Display, Group, Output, Tool, ToolError, ToolKind, arg, blocking, def, obj};
use crate::sandbox::{Policy, open};
use grep_regex::{RegexMatcher, RegexMatcherBuilder};
use grep_searcher::{BinaryDetection, Searcher, SearcherBuilder, sinks::UTF8};
use ignore::WalkState;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

const GREP_MAX: usize = 200;

pub struct Grep {
    name: String,
}

impl Grep {
    pub const NAME: &str = "grep";

    /// The tool under another wire name (`[tools] search_name`).
    pub fn named(name: &str) -> Self {
        Self { name: name.into() }
    }
}

impl Default for Grep {
    fn default() -> Self {
        Self::named(Self::NAME)
    }
}

#[async_trait::async_trait]
impl Tool for Grep {
    fn name(&self) -> &str {
        &self.name
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Search
    }
    fn group(&self) -> Group {
        Group::Core
    }
    fn definition(&self) -> Value {
        def(
            &self.name,
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
        let policy = ctx.can_read(&self.name, &a, "path").await?;
        blocking(ctx, || run(ctx, &policy, &a))
    }
}

fn run(ctx: &Ctx, policy: &Policy, a: &Value) -> Result<String, ToolError> {
    let pattern = arg(a, "pattern").ok_or("pattern is required")?;
    let root = arg(a, "path").map(|p| resolve(ctx, p)).unwrap_or_else(|| ctx.cwd.clone());
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
    let found = if open::metadata(policy, &root).is_ok_and(|m| m.is_file()) {
        let mut found = Found::default();
        found.add(&root, search_file(ctx, &mut searcher(), &matcher, policy, &root));
        found
    } else {
        search_tree(ctx, &root, policy, &matcher, include.as_ref())
    };
    let total = found.total;
    if total == 0 {
        return Ok("no matches".into());
    }
    let mut out = found.top.into_lines();
    if total > out.len() {
        out.push(format!(
            "… {total} matches in {} files, showing {}. Narrow the pattern or path.",
            found.files,
            out.len()
        ));
    }
    Ok(out.join("\n"))
}

fn searcher() -> Searcher {
    SearcherBuilder::new().binary_detection(BinaryDetection::quit(0)).line_number(true).build()
}

/// What one file held: how many lines matched, and the first `GREP_MAX` of them.
struct Hits {
    count: usize,
    lines: Vec<String>,
}

fn search_file(ctx: &Ctx, searcher: &mut Searcher, matcher: &RegexMatcher, policy: &Policy, path: &Path) -> Hits {
    let mut hits = Hits { count: 0, lines: Vec::new() };
    // Opened through the policy, so a file swapped for a link to outside is not read.
    let Ok(file) = open::open_read(policy, path) else { return hits };
    let shown = display(ctx, path);
    let _ = searcher.search_file(
        matcher,
        &file,
        UTF8(|ln, line| {
            hits.count += 1;
            if hits.lines.len() < GREP_MAX {
                hits.lines.push(format!("{shown}:{ln}: {}", crate::llm::truncate(line.trim_end(), 300)));
            }
            Ok(true)
        }),
    );
    hits
}

/// The lines that can still reach the output: the files that sort first, until
/// they hold `GREP_MAX` lines between them. A file that sorts after those can
/// never show, so it is dropped, and what is held stays small however many
/// files match.
#[derive(Default)]
struct Top {
    files: BTreeMap<PathBuf, Vec<String>>,
    lines: usize,
}

impl Top {
    fn offer(&mut self, path: &Path, lines: Vec<String>) {
        if lines.is_empty()
            || (self.lines >= GREP_MAX && self.files.last_key_value().is_some_and(|(last, _)| path > last.as_path()))
        {
            return;
        }
        self.lines += lines.len();
        self.files.insert(path.to_path_buf(), lines);
        while let Some((_, last)) = self.files.last_key_value() {
            if self.lines - last.len() < GREP_MAX {
                break;
            }
            self.lines -= last.len();
            self.files.pop_last();
        }
    }

    fn into_lines(self) -> Vec<String> {
        self.files.into_values().flatten().take(GREP_MAX).collect()
    }
}

#[derive(Default)]
struct Found {
    top: Top,
    total: usize,
    files: usize,
}

impl Found {
    fn add(&mut self, path: &Path, hits: Hits) {
        if hits.count > 0 {
            self.total += hits.count;
            self.files += 1;
            self.top.offer(path, hits.lines);
        }
    }
}

/// Walk `root` and search what the walk finds, in parallel, each worker with
/// its own searcher.
fn search_tree(
    ctx: &Ctx,
    root: &Path,
    policy: &Policy,
    matcher: &RegexMatcher,
    include: Option<&globset::GlobMatcher>,
) -> Found {
    let top = Mutex::new(Top::default());
    let (total, files) = (AtomicUsize::new(0), AtomicUsize::new(0));
    walker(root, false).build_parallel().run(|| {
        let mut searcher = searcher();
        let (top, total, files) = (&top, &total, &files);
        Box::new(move |e| {
            let Ok(e) = e else { return WalkState::Continue };
            if !e.file_type().is_some_and(|t| t.is_file()) {
                return WalkState::Continue;
            }
            if let Some(inc) = include {
                let rel = e.path().strip_prefix(root).unwrap_or(e.path());
                if !inc.is_match(rel) {
                    return WalkState::Continue;
                }
            }
            let hits = search_file(ctx, &mut searcher, matcher, policy, e.path());
            if hits.count > 0 {
                total.fetch_add(hits.count, Ordering::Relaxed);
                files.fetch_add(1, Ordering::Relaxed);
                // A poisoned lock is a panic in another worker.
                top.lock().unwrap().offer(e.path(), hits.lines);
            }
            WalkState::Continue
        })
    });
    // The workers are done, so the lock is free; poisoned only if one panicked.
    Found { top: top.into_inner().unwrap(), total: total.into_inner(), files: files.into_inner() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(name: &str, n: usize) -> Vec<String> {
        (1..=n).map(|i| format!("{name}:{i}")).collect()
    }

    #[test]
    fn top_keeps_the_first_lines_in_path_order_whatever_the_arrival_order() {
        // 100 files of 7 lines: 700 lines offered, 200 can show.
        let names: Vec<String> = (0..100).map(|i| format!("dir/f{i:03}.rs")).collect();
        let want: Vec<String> = names.iter().flat_map(|n| lines(n, 7)).take(GREP_MAX).collect();
        for step in [1, 7, 13, 37] {
            let mut top = Top::default();
            // A scrambled order: stride through the names, as threads finishing out of order would.
            for k in 0..names.len() {
                let n = &names[(k * step) % names.len()];
                top.offer(Path::new(n), lines(n, 7));
            }
            assert!(top.lines - top.files.last_key_value().map_or(0, |(_, v)| v.len()) < GREP_MAX);
            assert!(top.files.len() <= GREP_MAX / 7 + 2, "held {} files", top.files.len());
            assert_eq!(top.into_lines(), want, "stride {step}");
        }
    }
}
