//! The crate-count ratchet: how many external packages `Cargo.lock` holds,
//! against `[lockfile]` in `docs/ratchets.toml`. It is the early half of the
//! binary-size ratchet (`cargo xtask size`, which needs a release build): a
//! new dependency drags in its own tree, the count moves at once, and `check
//! --quick` says so before anything is compiled. Exact, like the line counts,
//! since the lockfile is deterministic; workspace members have no `source`
//! and do not count.

use super::ratchet::RATCHETS;
use serde::Deserialize;
use std::path::Path;

/// `[lockfile]` in `docs/ratchets.toml`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Lockfile {
    packages: usize,
}

/// Packages in `Cargo.lock` that come from a registry or a git source, or
/// `None` if there is no readable lockfile (a tree under test has none).
fn external(root: &Path) -> Option<usize> {
    let text = std::fs::read_to_string(root.join("Cargo.lock")).ok()?;
    let lock: toml::Table = text.parse().ok()?;
    let packages = lock.get("package")?.as_array()?;
    Some(packages.iter().filter(|p| p.get("source").is_some()).count())
}

pub(super) fn problems(root: &Path, baseline: Option<&Lockfile>) -> Vec<String> {
    let Some(n) = external(root) else { return vec![] };
    match baseline.map(|b| b.packages) {
        None => vec![format!("{RATCHETS}: no [lockfile] section; add `packages = {n}`")],
        Some(was) if n > was => vec![format!(
            "Cargo.lock: {n} external crates, up from {was} in {RATCHETS}. A new dependency brings its whole tree and grows the binary: \
             stop, see what came in with `git diff main -- Cargo.lock`, and tell the user which dependency, why it is worth it \
             (AGENTS.md convention 8) and how many crates it brings, before setting `packages = {n}` under [lockfile]"
        )],
        Some(was) if n < was => {
            vec![format!(
                "{RATCHETS}: Cargo.lock shrank from {was} to {n} external crates; lower `packages` under [lockfile] to {n}"
            )]
        }
        Some(_) => vec![],
    }
}
