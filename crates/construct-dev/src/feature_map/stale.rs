//! Staleness: a `verified` entry is proven only for the code it was driven
//! against. Its dependencies are the recipes it names plus its `depends:`
//! paths. `verified_deps` fingerprints their contents at `verified_at`; the
//! entry is stale once the fingerprint at HEAD differs. Content, not history,
//! because a squash merge drops `verified_at` from `main`'s history (and, once
//! the branch is deleted, from the repo) while leaving the driven tree intact.
//! The commit that bumps `verified_at` touches only the map, so it never trips
//! the check itself (ADR 0023, ADR 0031).

use super::{Entry, MAP, RECIPES, listed, recipe_refs};
use std::collections::BTreeSet;

/// The git questions the gate asks, injected so tests need no repository.
pub struct Git<'a> {
    /// Whether a `verified_at` names a commit in this repo.
    pub is_commit: &'a dyn Fn(&str) -> bool,
    /// The object id of `path` at `rev`, or `None` if it is not there.
    pub object: &'a dyn Fn(&str, &str) -> Option<String>,
    /// The commits after `since`, up to HEAD, that changed `path`; empty if it
    /// is the same at HEAD as at `since`.
    pub changed_since: &'a dyn Fn(&str, &str) -> Vec<String>,
}

/// A `verified` entry's dependencies: the recipes it names and its
/// `depends:` paths (files or directories, from the repo root).
fn dependencies(e: &Entry) -> Vec<String> {
    let mut deps: Vec<String> = recipe_refs(&e.text).into_iter().map(|r| format!("{RECIPES}/{r}")).collect();
    deps.extend(listed(e, "depends"));
    deps
}

/// 64-bit FNV-1a over each dependency's `path object` line, in hex: stable
/// across toolchains, which `std`'s hasher does not promise. Collisions only
/// matter by accident here, never by attack.
fn fingerprint(deps: &[String], object: impl Fn(&str) -> Option<String>) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for dep in deps {
        let line = format!("{dep} {}\n", object(dep).unwrap_or_else(|| "-".into()));
        for b in line.bytes() {
            h = (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
        }
    }
    format!("{h:016x}")
}

/// A `verified` entry whose dependencies at HEAD differ from what was driven
/// is no longer proven. The record of what was driven is `verified_deps`,
/// checked against `verified_at` while that commit is in the repo; after a
/// squash merge it may not be, and the fingerprint alone decides.
pub(super) fn stale(entries: &[Entry], git: &Git, bad: &mut Vec<String>) {
    for e in entries.iter().filter(|e| e.front.get("status").is_some_and(|s| s == "verified")) {
        let Some(sha) = e.front.get("verified_at").filter(|sha| !sha.is_empty()) else { continue };
        let deps = dependencies(e);
        let at = |rev: &str| fingerprint(&deps, |p| (git.object)(rev, p));
        let driven = (git.is_commit)(sha).then(|| at(sha));
        let at_file = |what: String| format!("{MAP}/{}: {what}", e.file);
        let recorded = match (e.front.get("verified_deps").filter(|r| !r.is_empty()), &driven) {
            (None, Some(d)) => {
                bad.push(at_file(format!("verified with no verified_deps; at {sha} it is `verified_deps: {d}`")));
                continue;
            }
            (None, None) => continue, // `structure` reports the missing commit
            (Some(r), Some(d)) if r != d => {
                bad.push(at_file(format!(
                    "verified_deps `{r}` is not the dependencies at {sha}, which are `{d}`; re-drive and record that"
                )));
                continue;
            }
            (Some(r), _) => r,
        };
        if at("HEAD") == *recorded {
            continue;
        }
        if driven.is_none() {
            bad.push(at_file(format!(
                "verified at {sha} (not in this repo), but its dependencies changed since; re-drive the recipe and bump verified_at and verified_deps, or set status: built"
            )));
            continue;
        }
        let mut commits = BTreeSet::new();
        let mut moved = Vec::new();
        for dep in deps.iter().filter(|d| (git.object)(sha, d) != (git.object)("HEAD", d)) {
            commits.extend((git.changed_since)(sha, dep));
            moved.push(dep.as_str());
        }
        let n = commits.len();
        bad.push(at_file(format!(
            "verified at {sha}, but {} changed since ({n} commit{}); re-drive the recipe and bump verified_at and verified_deps, or set status: built",
            moved.join(", "),
            if n == 1 { "" } else { "s" },
        )));
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{GOOD, GOOD_DEPS, tree, write};
    use super::super::{MAP, RECIPES, problems};
    use super::Git;

    #[test]
    fn a_verified_entry_whose_dependencies_moved_is_stale() {
        let root = tree("stale");
        write(&root, &format!("{MAP}/turn.md"), GOOD);
        let theme = "crates/construct/src/tui/theme.rs";
        let object = |rev: &str, path: &str| match (rev, path) {
            ("HEAD", p) if p == theme || p.ends_with("recipes/turn") => Some(format!("new {p}")),
            (_, p) => Some(format!("old {p}")),
        };
        let changed_since = |sha: &str, path: &str| {
            assert_eq!(sha, "abc1234");
            match path {
                p if p == theme => vec!["c1".into(), "c2".into()],
                p if p.ends_with("recipes/turn") => vec!["c2".into()],
                _ => Vec::new(),
            }
        };
        let git = Git { is_commit: &|_| true, object: &object, changed_since: &changed_since };
        assert_eq!(
            problems(&root, &git),
            [format!(
                "{MAP}/turn.md: verified at abc1234, but {RECIPES}/turn, {theme} changed since (2 commits); \
                 re-drive the recipe and bump verified_at and verified_deps, or set status: built"
            )]
        );

        // The same move after a squash merge took `abc1234` out of the repo.
        let git = Git { is_commit: &|_| false, object: &object, changed_since: &|_, _| panic!("no history to ask") };
        let bad = problems(&root, &git);
        assert_eq!(bad.len(), 1, "{bad:#?}");
        assert!(bad[0].contains("verified at abc1234 (not in this repo), but its dependencies changed"), "{bad:#?}");

        let built = GOOD
            .replace("status: verified", "status: built")
            .replace("verified_at: abc1234\n", "")
            .replace(&format!("verified_deps: {GOOD_DEPS}\n"), "");
        write(&root, &format!("{MAP}/turn.md"), &built);
        assert_eq!(problems(&root, &git), Vec::<String>::new());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_fingerprint_must_match_verified_at() {
        let root = tree("deps");
        let unchanged = |_: &str, p: &str| Some(format!("old {p}"));
        let git = Git { is_commit: &|_| true, object: &unchanged, changed_since: &|_, _| Vec::new() };
        write(&root, &format!("{MAP}/turn.md"), &GOOD.replace(GOOD_DEPS, "0000000000000000"));
        let bad = problems(&root, &git);
        assert!(
            bad[0].contains(&format!(
                "verified_deps `0000000000000000` is not the dependencies at abc1234, which are `{GOOD_DEPS}`"
            )),
            "{bad:#?}"
        );
        write(&root, &format!("{MAP}/turn.md"), &GOOD.replace(&format!("verified_deps: {GOOD_DEPS}\n"), ""));
        let bad = problems(&root, &git);
        assert!(
            bad[0].contains(&format!("verified with no verified_deps; at abc1234 it is `verified_deps: {GOOD_DEPS}`")),
            "{bad:#?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
