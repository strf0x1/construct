//! Ratchets: counts of known debt in `docs/ratchets.toml` that must equal
//! the real count exactly. More fails because the debt grew; fewer fails too,
//! asking for the lower baseline, so an improvement is locked in by the PR
//! that made it.
//!
//! One is kept, `[lines]`: each source file under `crates/*/src` over the
//! 300-line soft ceiling (AGENTS.md convention 3), counting only lines
//! outside `#[cfg(test)]` items, with `tests.rs` files left out. A count of
//! tool names retyped as literals was tried and dropped: nearly every string
//! spelling `"read"` or `"memory"` meant something else (an access mode, a
//! directory), and a ratchet that is mostly noise teaches agents to bump the
//! baseline without reading it.

use super::lex::{lex, strip_tests};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

pub(super) const RATCHETS: &str = "docs/ratchets.toml";
pub(super) const SOFT_LIMIT: usize = 300;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Ratchets {
    #[serde(default)]
    lines: BTreeMap<String, usize>,
}

/// Non-test lines of every file over the soft limit, by repo-relative path.
fn long_files(root: &Path) -> BTreeMap<String, usize> {
    let mut out = BTreeMap::new();
    for f in crate::xtask::rust_sources(&root.join("crates")) {
        let Ok(text) = std::fs::read_to_string(&f) else { continue };
        if f.file_name().is_some_and(|n| n == "tests.rs") {
            continue;
        }
        let total = text.lines().count();
        let (_, test) = strip_tests(lex(&text), total);
        let n = test.iter().filter(|t| !**t).count();
        if n > SOFT_LIMIT {
            out.insert(f.strip_prefix(root).unwrap_or(&f).display().to_string(), n);
        }
    }
    out
}

pub(super) fn problems(root: &Path) -> Vec<String> {
    let r: Ratchets = match std::fs::read_to_string(root.join(RATCHETS)) {
        Err(e) => return vec![format!("{RATCHETS}: cannot read it ({e})")],
        Ok(text) => match toml::from_str(&text) {
            Ok(r) => r,
            Err(e) => return vec![format!("{RATCHETS}: {}", e.message())],
        },
    };
    let mut bad = Vec::new();

    let real = long_files(root);
    for (file, &n) in &real {
        match r.lines.get(file) {
            None => bad.push(format!(
                "{file}: {n} non-test lines, over the {SOFT_LIMIT}-line soft limit; split it by concern, or if it is one concern add `\"{file}\" = {n}` under [lines] in {RATCHETS}"
            )),
            Some(&was) if n > was => bad.push(format!(
                "{RATCHETS}: {file} grew from {was} to {n} non-test lines; split it by concern instead of adding to it"
            )),
            Some(&was) if n < was => {
                bad.push(format!("{RATCHETS}: {file} shrank from {was} to {n} non-test lines; lower the baseline to {n}"))
            }
            Some(_) => {}
        }
    }
    for file in r.lines.keys().filter(|f| !real.contains_key(*f)) {
        bad.push(format!(
            "{RATCHETS}: {file} is no longer over the {SOFT_LIMIT}-line soft limit (or is gone); delete its line"
        ));
    }

    bad
}
