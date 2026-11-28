//! Pure filesystem and encoding helpers shared by the built-in file tools.
//! Path resolution and display relative to `cwd` live on `Ctx`, so this module
//! carries no dependency on `Ctx` or `shell`.

use crate::tools::{SHELL_NAME, ToolError};
use ignore::WalkBuilder;
use std::path::Path;

/// A UTF-16 byte order mark. UTF-16 text is full of NUL bytes, so without
/// this check it would be called binary.
pub(super) fn is_utf16(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0xff, 0xfe]) || bytes.starts_with(&[0xfe, 0xff])
}

/// Text the file tools can't round-trip: UTF-16, or bytes that aren't UTF-8
/// (Latin-1, Windows-1252, Shift-JIS…). Writing either back as UTF-8 would
/// corrupt every line the edit didn't touch, so refuse and name the way out.
pub(super) fn encoding_error(shown: &str, utf16: bool) -> ToolError {
    if utf16 {
        ToolError::new(
            "utf16",
            format!(
                "{shown} is UTF-16 text, which read and edit can't handle. Use {SHELL_NAME} instead: view it with `iconv -f UTF-16 -t UTF-8 {shown}`, and edit it with shell tools or convert it to UTF-8 first.",
            ),
        )
    } else {
        ToolError::new(
            "not_utf8",
            format!(
                "{shown} is not valid UTF-8 (probably Latin-1 or another legacy encoding), so edit can't change it without corrupting it. Edit it with {SHELL_NAME} instead (sed works byte for byte), or convert it to UTF-8 first with iconv.",
            ),
        )
    }
}

/// A walk below `root` that never follows a link: not out of the root, not
/// into a directory swapped for one. (Opening what it finds goes through
/// `sandbox::open`, which is the check that holds against a swap.)
pub(super) fn walker(root: &Path, hidden: bool) -> WalkBuilder {
    let mut w = WalkBuilder::new(root);
    w.follow_links(false);
    w.hidden(!hidden).git_ignore(true).git_global(true).git_exclude(true).parents(true).require_git(false);
    w.filter_entry(|e| e.file_name() != ".git");
    w
}

pub(super) fn display(cwd: &Path, p: &Path) -> String {
    p.strip_prefix(cwd).map(|r| r.display().to_string()).unwrap_or_else(|_| p.display().to_string())
}
