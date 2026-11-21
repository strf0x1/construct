//! A merge commit binds Git's worktree-local MERGE_HEAD to the same approval
//! as the staged tree. Use Git's index lock while installing the result.

use super::super::runner::Executable;
use super::{context::CommitContext, oid};
use construct_core::tools::{Ctx, ToolError};
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Read};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

const MAX_BYTES: u64 = 64 * 1024;

/// Git writes one full object ID per line, in parent order.
pub(super) fn parents(git_dir: &str, oid_len: usize) -> Result<Vec<String>, ToolError> {
    let git_dir = Path::new(git_dir);
    let open = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(git_dir.join("MERGE_HEAD"));
    let file = match open {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return Err("cannot read Git's merge parents".into()),
    };
    if !file.metadata().is_ok_and(|m| m.is_file()) {
        return Err("Git's merge parents must be a regular file".into());
    }
    match fs::symlink_metadata(git_dir.join("MERGE_AUTOSTASH")) {
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(_) => return Err("cannot check for a merge autostash".into()),
        Ok(_) => return Err("Finish this merge in Git to restore its autostash.".into()),
    }
    let mut input = String::new();
    file.take(MAX_BYTES + 1).read_to_string(&mut input).map_err(|_| "cannot read Git's merge parents")?;
    if input.is_empty() || input.len() as u64 > MAX_BYTES {
        return Err("invalid size for Git's merge parents".into());
    }
    input
        .lines()
        .map(|line| if line.len() == oid_len { oid(line) } else { Err("invalid merge parent identity".into()) })
        .collect()
}

pub(super) async fn verify_parents(
    ctx: &Ctx,
    exe: &Executable,
    review: &CommitContext,
    head: Option<&str>,
    parents: &[String],
) -> Result<(), ToolError> {
    let head = head.ok_or("a merge requires an existing branch commit")?;
    let mut seen = std::collections::HashSet::from([head]);
    if !parents.iter().all(|parent| seen.insert(parent.as_str())) {
        return Err("Git's merge parents contain a duplicate commit".into());
    }
    let input = parents.join("\n") + "\n";
    let types = review.git(ctx, exe, &["cat-file", "--batch-check=%(objecttype)"], input.as_bytes(), &[]).await?;
    if !types.lines().eq(std::iter::repeat_n("commit", parents.len())) {
        return Err("Git's merge parents must name available commits".into());
    }
    let ancestry = review.git(ctx, exe, &["rev-list", "--parents", "-1", head], &[], &[]).await?;
    if ancestry.split_whitespace().skip(2).eq(parents.iter().map(String::as_str)) {
        return Err("HEAD already records these merge parents. Run git merge --quit to clear the merge state.".into());
    }
    Ok(())
}

pub(super) struct IndexLock {
    path: PathBuf,
    file: File,
}

impl IndexLock {
    pub(super) fn acquire(git_dir: &Path) -> Result<Self, ToolError> {
        if git_dir.canonicalize().ok().as_deref() != Some(git_dir) {
            return Err("Git's index directory changed. Prepare and review again.".into());
        }
        let path = git_dir.join("index.lock");
        let file = OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path).map_err(|error| {
            if error.kind() == ErrorKind::AlreadyExists {
                ToolError::new("changed", "Git's index is locked. Finish the other Git operation and review again.")
            } else {
                ToolError::from("cannot lock Git's index")
            }
        })?;
        Ok(Self { path, file })
    }
}

impl Drop for IndexLock {
    fn drop(&mut self) {
        if let (Ok(locked), Ok(current)) = (self.file.metadata(), fs::symlink_metadata(&self.path))
            && (locked.dev(), locked.ino()) == (current.dev(), current.ino())
        {
            let _ = fs::remove_file(&self.path);
        }
    }
}
