//! The `map` gate's git questions, asked of the repository at `root`.
//!
//! Every answer is about content at a revision (`rev:path`), so the gate holds
//! after a squash merge; `changed_since` reads history only to name the
//! commits behind a change it has already found by content.

use std::path::Path;
use std::process::{Command, Stdio};

fn git(root: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new("git");
    cmd.args(args).current_dir(root).stderr(Stdio::null());
    cmd
}

/// Whether `sha` names a commit in the repository at `root`.
pub fn is_commit(root: &Path, sha: &str) -> bool {
    let rev = format!("{sha}^{{commit}}");
    git(root, &["cat-file", "-e", &rev]).status().is_ok_and(|s| s.success())
}

/// The blob or tree id of `path` at `rev`, or `None` if it is not there.
pub fn object(root: &Path, rev: &str, path: &str) -> Option<String> {
    let spec = format!("{rev}:{path}");
    let out = git(root, &["rev-parse", "--verify", "--quiet", &spec]).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The non-merge commits in `sha..HEAD` that touch `path`, or nothing if
/// `path` is the same at HEAD as at `sha`.
pub fn changed_since(root: &Path, sha: &str, path: &str) -> Vec<String> {
    if git(root, &["diff", "--quiet", sha, "HEAD", "--", path]).status().is_ok_and(|s| s.success()) {
        return Vec::new();
    }
    let range = format!("{sha}..HEAD");
    let out = git(root, &["log", "--no-merges", "--format=%H", &range, "--", path]).output();
    out.map(|o| String::from_utf8_lossy(&o.stdout).lines().map(String::from).collect()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::super::tests::{GOOD, GOOD_DEPS, tree, write};
    use super::super::{Git, MAP, problems};
    use super::{changed_since, git, is_commit, object};
    use std::path::Path;

    fn run(root: &Path, args: &[&str]) -> String {
        let out = git(root, args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?} failed");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn commit(root: &Path, msg: &str) -> String {
        run(root, &["add", "-A"]);
        run(root, &["commit", "-q", "--allow-empty", "-m", msg]);
        run(root, &["rev-parse", "--short", "HEAD"])
    }

    fn gate(root: &Path) -> Vec<String> {
        problems(
            root,
            &Git {
                is_commit: &|sha| is_commit(root, sha),
                object: &|rev, p| object(root, rev, p),
                changed_since: &|sha, p| changed_since(root, sha, p),
            },
        )
    }

    /// Record a verification of `turn` at `sha`, taking `verified_deps` from
    /// the gate's own message, as an author would.
    fn verify_at(root: &Path, sha: &str) {
        let entry = format!("{MAP}/turn.md");
        write(root, &entry, &GOOD.replace("abc1234", sha).replace(&format!("verified_deps: {GOOD_DEPS}\n"), ""));
        let bad = gate(root);
        let deps = bad
            .iter()
            .find_map(|p| p.split("`verified_deps: ").nth(1)?.split('`').next())
            .unwrap_or_else(|| panic!("no verified_deps offered: {bad:#?}"));
        write(root, &entry, &GOOD.replace("abc1234", sha).replace(GOOD_DEPS, deps));
    }

    const THEME: &str = "crates/construct/src/tui/theme.rs";

    /// A PR changes a dependency, re-drives the recipe on its branch and bumps
    /// `verified_at` to that branch commit, then is squash-merged and its
    /// branch deleted: `main` never has `verified_at`, only the tree it drove.
    #[test]
    fn a_verification_survives_a_squash_merge_but_not_a_later_change() {
        let root = tree("squash");
        run(&root, &["init", "-q", "-b", "main"]);
        let base = commit(&root, "base");
        verify_at(&root, &base);
        commit(&root, "verify turn");
        assert_eq!(gate(&root), Vec::<String>::new(), "verified on main");

        run(&root, &["checkout", "-q", "-b", "pr"]);
        write(&root, THEME, "pub const GREEN: Color = Color::Rgb(158, 206, 106); // tweaked\n");
        let driven = commit(&root, "change the theme");
        assert!(gate(&root)[0].contains(&format!("{THEME} changed since (1 commit)")), "the change is caught");
        verify_at(&root, &driven);
        commit(&root, "re-verify turn");
        assert_eq!(gate(&root), Vec::<String>::new(), "re-verified on the branch");

        run(&root, &["checkout", "-q", "main"]);
        run(&root, &["merge", "-q", "--squash", "pr"]);
        commit(&root, "squash");
        run(&root, &["branch", "-q", "-D", "pr"]);
        run(&root, &["reflog", "expire", "--expire=now", "--all"]);
        run(&root, &["gc", "-q", "--prune=now"]);
        assert!(!is_commit(&root, &driven), "the branch commit is gone, as in a fresh clone of main");
        assert_eq!(gate(&root), Vec::<String>::new(), "the squash keeps the proof");

        write(&root, THEME, "pub const GREEN: Color = Color::Rgb(158, 206, 106); // again\n");
        commit(&root, "change the theme after verification");
        let bad = gate(&root);
        assert_eq!(bad.len(), 1, "{bad:#?}");
        assert!(
            bad[0].contains(&format!("verified at {driven} (not in this repo), but its dependencies changed")),
            "{bad:#?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
