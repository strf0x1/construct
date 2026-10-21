//! Path helpers shared by the file tools.

use super::{Ctx, ToolError};
use ignore::WalkBuilder;
use std::path::{Path, PathBuf};

/// A UTF-16 byte order mark. UTF-16 text is full of NUL bytes, so without
/// this check it would be called binary.
pub fn is_utf16(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0xff, 0xfe]) || bytes.starts_with(&[0xfe, 0xff])
}

/// Text the file tools can't round-trip: UTF-16, or bytes that aren't UTF-8
/// (Latin-1, Windows-1252, Shift-JIS…). Writing either back as UTF-8 would
/// corrupt every line the edit didn't touch, so refuse and name the way out.
pub fn encoding_error(shown: &str, utf16: bool) -> ToolError {
    if utf16 {
        ToolError::new(
            "utf16",
            format!(
                "{shown} is UTF-16 text, which read and edit can't handle. Use shell instead: view it with `iconv -f UTF-16 -t UTF-8 {shown}`, and edit it with shell tools or convert it to UTF-8 first."
            ),
        )
    } else {
        ToolError::new(
            "not_utf8",
            format!(
                "{shown} is not valid UTF-8 (probably Latin-1 or another legacy encoding), so edit can't change it without corrupting it. Edit it with shell instead (sed works byte for byte), or convert it to UTF-8 first with iconv."
            ),
        )
    }
}

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
