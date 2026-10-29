//! Staleness: a `verified` entry is proven only for the code it was driven
//! against. Its dependencies are the recipes it names plus its `depends:`
//! paths; a non-merge commit touching one after `verified_at` makes the claim
//! stale. The commit that bumps `verified_at` touches only the map, so it
//! never trips the check itself (ADR 0023).

use super::{Entry, MAP, RECIPES, listed, recipe_refs};
use std::collections::BTreeSet;

/// The git questions the gate asks, injected so tests need no repository.
pub struct Git<'a> {
    /// Whether a `verified_at` names a commit in this repo.
    pub is_commit: &'a dyn Fn(&str) -> bool,
    /// The non-merge commits after `since`, up to HEAD, that touch `path`.
    pub changed_since: &'a dyn Fn(&str, &str) -> Vec<String>,
}

/// A `verified` entry's dependencies: the recipes it names and its
/// `depends:` paths (files or directories, from the repo root).
fn dependencies(e: &Entry) -> Vec<String> {
    let mut deps: Vec<String> = recipe_refs(&e.text).into_iter().map(|r| format!("{RECIPES}/{r}")).collect();
    deps.extend(listed(e, "depends"));
    deps
}

/// A `verified` entry whose dependencies changed after `verified_at` is no
/// longer proven. A `verified_at` that is not a commit is reported by
/// `structure` instead.
pub(super) fn stale(entries: &[Entry], git: &Git, bad: &mut Vec<String>) {
    for e in entries.iter().filter(|e| e.front.get("status").is_some_and(|s| s == "verified")) {
        let Some(sha) = e.front.get("verified_at").filter(|sha| (git.is_commit)(sha)) else { continue };
        let mut commits = BTreeSet::new();
        let mut moved = Vec::new();
        for dep in dependencies(e) {
            let since = (git.changed_since)(sha, &dep);
            if !since.is_empty() {
                commits.extend(since);
                moved.push(dep);
            }
        }
        if !moved.is_empty() {
            let n = commits.len();
            bad.push(format!(
                "{MAP}/{}: verified at {sha}, but {} changed since ({n} commit{}); re-drive the recipe and bump verified_at, or set status: built",
                e.file,
                moved.join(", "),
                if n == 1 { "" } else { "s" },
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{GOOD, tree, write};
    use super::super::{MAP, RECIPES, problems};
    use super::Git;

    #[test]
    fn a_verified_entry_whose_dependencies_moved_is_stale() {
        let root = tree("stale");
        write(&root, &format!("{MAP}/turn.md"), GOOD);
        let changed_since = |sha: &str, path: &str| {
            assert_eq!(sha, "abc1234");
            match path {
                "crates/construct/src/tui/theme.rs" => vec!["c1".into(), "c2".into()],
                p if p.ends_with("recipes/turn") => vec!["c2".into()],
                _ => Vec::new(),
            }
        };
        let bad = problems(&root, &Git { is_commit: &|_| true, changed_since: &changed_since });
        assert_eq!(
            bad,
            [format!(
                "{MAP}/turn.md: verified at abc1234, but {RECIPES}/turn, crates/construct/src/tui/theme.rs changed \
                 since (2 commits); re-drive the recipe and bump verified_at, or set status: built"
            )]
        );

        let built = GOOD.replace("status: verified", "status: built").replace("verified_at: abc1234\n", "");
        write(&root, &format!("{MAP}/turn.md"), &built);
        assert_eq!(problems(&root, &Git { is_commit: &|_| true, changed_since: &changed_since }), Vec::<String>::new());
        let _ = std::fs::remove_dir_all(&root);
    }
}
