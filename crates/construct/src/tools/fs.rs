//! Path helpers shared by the file tools.

use super::Ctx;
use ignore::WalkBuilder;
use std::path::{Path, PathBuf};

pub fn resolve(ctx: &Ctx, p: &str) -> PathBuf {
    let p = p.trim();
    let expanded = match p.strip_prefix("~/") {
        Some(rest) => std::env::var_os("HOME").map(|h| PathBuf::from(h).join(rest)).unwrap_or_else(|| PathBuf::from(p)),
        None => PathBuf::from(p),
    };
    if expanded.is_absolute() { expanded } else { ctx.cwd.join(expanded) }
}

pub(super) fn walker(root: &Path, hidden: bool) -> WalkBuilder {
    let mut w = WalkBuilder::new(root);
    w.hidden(!hidden).git_ignore(true).git_global(true).git_exclude(true).parents(true).require_git(false);
    w.filter_entry(|e| e.file_name() != ".git");
    w
}

pub(super) fn display(ctx: &Ctx, p: &Path) -> String {
    p.strip_prefix(&ctx.cwd).map(|r| r.display().to_string()).unwrap_or_else(|_| p.display().to_string())
}
