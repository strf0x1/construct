//! What a failed shell command's output means when the sandbox caused it.
//! The note is appended after the exit code, so the model reads it right at
//! the failure. A linked Git worktree whose metadata is out of reach gets a
//! note naming the `developer` operations instead, because widening the
//! sandbox to the source repository is the wrong fix (ADR 0030).

use super::developer::{Developer, MERGE_START, STAGE, STATUS};
use super::shell::Shell;
use crate::sandbox::Policy;
use std::io::Read as _;
use std::path::{Path, PathBuf};

/// Landlock denials look like ordinary permission errors; say why.
const DENIED: &str = "\n[sandbox: commands can only reach the workspace, system paths and toolchains; for scratch files use $TMPDIR, not /tmp]";

/// The note for a command run in `cwd` that exited non-zero with `out`, or
/// an empty string when the sandbox has nothing to do with it.
pub(super) fn hint(policy: &Policy, cwd: &Path, out: &str) -> String {
    if policy.off {
        return String::new();
    }
    if git_failed(out)
        && let Some(git_dir) = linked_git_dir(cwd)
        && policy.check_read(&git_dir).is_err()
    {
        return worktree_note(&git_dir);
    }
    if out.contains("Permission denied") || out.contains("Operation not permitted") {
        return DENIED.into();
    }
    String::new()
}

/// Git's errors when the gitdir a `.git` file names can't be read.
fn git_failed(out: &str) -> bool {
    out.contains("gitfile does not point to a valid repository") || out.contains("not a git repository")
}

fn worktree_note(git_dir: &Path) -> String {
    format!(
        "\n[sandbox: this is a linked Git worktree. Its Git metadata ({}) is outside the sandbox, so git in {shell} can't reach the repository, by design. Use the {dev} tool for this checkout: \"{STATUS}\", \"{STAGE}\" with exact file paths, then \"commit\"; \"{MERGE_START}\" starts a merge of one commit. Don't ask for access to the repository.]",
        git_dir.display(),
        shell = Shell::NAME,
        dev = Developer::NAME,
    )
}

/// The gitdir of the linked worktree `cwd` is in: the nearest `.git` above it
/// is a file whose `gitdir:` names `<common>/worktrees/<name>`. A directory
/// (an ordinary repository) or any other pointer (a submodule) is `None`.
fn linked_git_dir(cwd: &Path) -> Option<PathBuf> {
    let (root, dotgit) =
        cwd.ancestors().map(|dir| (dir, dir.join(".git"))).find(|(_, dotgit)| dotgit.symlink_metadata().is_ok())?;
    if !dotgit.symlink_metadata().ok()?.is_file() {
        return None;
    }
    let mut raw = String::new();
    std::fs::File::open(&dotgit).ok()?.take(8192).read_to_string(&mut raw).ok()?;
    let target = raw.lines().next()?.strip_prefix("gitdir: ")?.trim();
    let git_dir = root.join(target);
    let in_worktrees = git_dir.parent().and_then(Path::file_name).is_some_and(|name| name == "worktrees");
    in_worktrees.then_some(git_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A source repository's `.git` and a linked worktree beside it, laid out
    /// by hand: only the worktree's `.git` file matters to the hint.
    fn layout(name: &str, gitdir: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!("construct-shell-exit-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("worker/src")).unwrap();
        std::fs::create_dir_all(base.join("source/.git/worktrees/worker")).unwrap();
        std::fs::write(base.join("worker/.git"), format!("gitdir: {gitdir}\n")).unwrap();
        base
    }

    fn sandboxed(work: &Path) -> Policy {
        Policy { read: vec![Policy::real(work)], write: vec![Policy::real(work)], ..Policy::default() }
    }

    #[test]
    fn a_linked_worktree_out_of_reach_names_the_developer_operations() {
        let base = layout("linked", "../source/.git/worktrees/worker");
        let linked = base.join("worker");
        let out = "fatal: gitfile does not point to a valid repository: worker/.git\n[exit 128]";
        let note = hint(&sandboxed(&linked), &linked.join("src"), out);
        assert!(note.contains("linked Git worktree"), "{note}");
        for word in [Developer::NAME, STATUS, STAGE, MERGE_START, "commit"] {
            assert!(note.contains(word), "{word}: {note}");
        }
        // Once the repository is readable git works, so there is nothing to say.
        let mut wide = sandboxed(&linked);
        wide.read.push(Policy::real(&base));
        assert_eq!(hint(&wide, &linked, out), "");
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn other_failures_keep_their_notes() {
        // A submodule's pointer is not a linked worktree.
        let base = layout("other", "../source/.git/modules/worker");
        let linked = base.join("worker");
        let policy = sandboxed(&linked);
        assert_eq!(hint(&policy, &linked, "fatal: not a git repository"), "");
        assert_eq!(hint(&policy, &linked, "cat: /etc/shadow: Permission denied"), DENIED);
        let off = Policy { off: true, ..Policy::default() };
        assert_eq!(hint(&off, &linked, "Permission denied"), "");
        std::fs::remove_dir_all(base).unwrap();
    }
}
