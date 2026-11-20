//! Ratchets: counts of known debt in `docs/ratchets.toml` that must equal
//! the real count exactly. More fails because the debt grew; fewer fails too,
//! asking for the lower baseline, so an improvement is locked in by the PR
//! that made it.
//!
//! Two are kept. `[pairs]` lists, per crate, the module pairs that use each
//! other both ways (`a <-> b`, names sorted), found by the same scan as the
//! edge check. A pair not listed fails naming where each direction occurs; a
//! listed one that is gone fails until it is deleted. `may_use` makes a new
//! edge one line; this makes a new *back*-edge a visible, reviewed decision.
//!
//! `[lines]`: each source file under `crates/*/src` over the
//! 300-line soft ceiling (AGENTS.md convention 3), counting only lines
//! outside `#[cfg(test)]` items, with `tests.rs` files left out. A count of
//! tool names retyped as literals was tried and dropped: nearly every string
//! spelling `"read"` or `"memory"` meant something else (an access mode, a
//! directory), and a ratchet that is mostly noise teaches agents to bump the
//! baseline without reading it.

use super::edges::krates;
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
    #[serde(default)]
    pairs: BTreeMap<String, Vec<String>>,
}

/// Per crate, each two-way module pair as `a <-> b` with where each direction
/// first occurs.
fn two_way(root: &Path) -> BTreeMap<String, BTreeMap<String, String>> {
    let ks = krates(root);
    let (idents, target_mods) = super::names(&ks);
    let mut out = BTreeMap::new();
    for k in &ks {
        let (intra, _) = super::graph(k, &idents, &target_mods, &mut Vec::new());
        let mut first: BTreeMap<(&str, &str), String> = BTreeMap::new();
        for e in &intra {
            first.entry((&e.from, &e.to)).or_insert_with(|| format!("{}:{}", e.file, e.line));
        }
        let mut pairs = BTreeMap::new();
        for (&(a, b), at) in &first {
            if a < b
                && let Some(back) = first.get(&(b, a))
            {
                pairs.insert(format!("{a} <-> {b}"), format!("{a} uses {b} at {at}; {b} uses {a} at {back}"));
            }
        }
        if !pairs.is_empty() {
            out.insert(k.name.clone(), pairs);
        }
    }
    out
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

    let real = two_way(root);
    let none = BTreeMap::new();
    for (krate, found) in &real {
        let listed = r.pairs.get(krate);
        for (pair, why) in found {
            if !listed.is_some_and(|l| l.contains(pair)) {
                bad.push(format!(
                    "{krate}: new two-way pair {pair} ({why}); cut the back-edge, or if it is intended add \"{pair}\" under [pairs] \"{krate}\" in {RATCHETS}"
                ));
            }
        }
    }
    for (krate, listed) in &r.pairs {
        let found = real.get(krate).unwrap_or(&none);
        for pair in listed.iter().filter(|p| !found.contains_key(*p)) {
            bad.push(format!(
                "{RATCHETS}: [pairs] {krate}: {pair} is no longer a two-way pair (or is gone); delete it from the list"
            ));
        }
    }

    bad
}
