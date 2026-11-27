//! What a failed shell command's output means when the sandbox caused it.
//! The note is appended after the exit code, so the model reads it right at
//! the failure. A linked Git worktree whose metadata is out of reach gets the
//! note a tool source supplies (the `developer` operations, ADR 0030) instead,
//! because widening the sandbox to the source repository is the wrong fix.
//! Core only finds the worktree; it never names the tools that work there.

use crate::sandbox::Policy;
use crate::tools::ToolSources;
use std::io::Read as _;
use std::path::{Path, PathBuf};

/// Landlock denials look like ordinary permission errors; say why.
const DENIED: &str = "\n[sandbox: commands can only reach the workspace, system paths and toolchains; for scratch files use $TMPDIR, not /tmp]";

/// The note for a command run in `cwd` that exited non-zero with `out`, or
/// an empty string when the sandbox has nothing to do with it.
pub(super) fn hint(policy: &Policy, cwd: &Path, sources: &ToolSources, out: &str) -> String {
    if policy.off {
        return String::new();
    }
    if git_failed(out)
        && let Some(git_dir) = linked_git_dir(cwd)
        && policy.check_read(&git_dir).is_err()
        && let Some(note) = sources.worktree_note(&git_dir)
    {
        return note;
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

    struct Names;
    impl crate::tools::ToolSource for Names {
        fn tools(
            &self,
            _: &crate::config::ToolsConfig,
            _: &crate::tools::Deck,
            _: &crate::jobs::Jobs,
        ) -> anyhow::Result<Vec<std::sync::Arc<dyn crate::tools::Tool>>> {
            Ok(vec![])
        }
        fn worktree_note(&self, git_dir: &Path) -> Option<String> {
            Some(format!("\n[a source's note for {}]", git_dir.display()))
        }
    }

    fn sandboxed(work: &Path) -> Policy {
        Policy { read: vec![Policy::real(work)], write: vec![Policy::real(work)], ..Policy::default() }
    }

    #[test]
    fn a_linked_worktree_out_of_reach_gets_the_sources_note() {
        let base = layout("linked", "../source/.git/worktrees/worker");
        let linked = base.join("worker");
        let out = "fatal: gitfile does not point to a valid repository: worker/.git\n[exit 128]";
        let sources = ToolSources::default().with(Names);
        let note = hint(&sandboxed(&linked), &linked.join("src"), &sources, out);
        assert!(note.contains("a source's note"), "{note}");
        // With no source to name tools that work there, the failure is as any other.
        assert_eq!(hint(&sandboxed(&linked), &linked.join("src"), &ToolSources::default(), out), "");
        // Once the repository is readable git works, so there is nothing to say.
        let mut wide = sandboxed(&linked);
        wide.read.push(Policy::real(&base));
        assert_eq!(hint(&wide, &linked, &sources, out), "");
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn other_failures_keep_their_notes() {
        // A submodule's pointer is not a linked worktree.
        let base = layout("other", "../source/.git/modules/worker");
        let linked = base.join("worker");
        let policy = sandboxed(&linked);
        assert_eq!(hint(&policy, &linked, &ToolSources::default(), "fatal: not a git repository"), "");
        assert_eq!(hint(&policy, &linked, &ToolSources::default(), "cat: /etc/shadow: Permission denied"), DENIED);
        let off = Policy { off: true, ..Policy::default() };
        assert_eq!(hint(&off, &linked, &ToolSources::default(), "Permission denied"), "");
        std::fs::remove_dir_all(base).unwrap();
    }
}
