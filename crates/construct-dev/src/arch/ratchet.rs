//! Ratchets: counts of known debt in `docs/ratchets.toml` that must equal
//! the real count exactly. More fails because the debt grew; fewer fails too,
//! asking for the lower baseline, so an improvement is locked in by the PR
//! that made it.
//!
//! Three are kept. `[pairs]` lists, per crate, the module pairs that use each
//! other both ways (`a <-> b`, names sorted), found by the same scan as the
//! edge check. A pair not listed fails naming where each direction occurs; a
//! listed one that is gone fails until it is deleted. `may_use` makes a new
//! edge one line; this makes a new *back*-edge a visible, reviewed decision.
//!
//! `[cycles]` lists, per crate, every group of three or more modules joined
//! into one cycle (a strongly connected component, names sorted), because
//! pairs alone miss a loop like `a -> b -> c -> a` (ADR 0062). A component of
//! two is a pair, and `[pairs]` already says where its edges are. A cycle that
//! grows, merges with another or appears fails; one that shrinks, splits or
//! goes away fails until the list says so.
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
use std::collections::{BTreeMap, BTreeSet};
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
    #[serde(default)]
    cycles: BTreeMap<String, Vec<String>>,
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

/// Per crate, each cycle of three or more modules as its sorted names joined by `, `.
fn cycles(root: &Path) -> BTreeMap<String, BTreeSet<String>> {
    let ks = krates(root);
    let (idents, target_mods) = super::names(&ks);
    let mut out = BTreeMap::new();
    for k in &ks {
        let (intra, _) = super::graph(k, &idents, &target_mods, &mut Vec::new());
        let mut next: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
        for e in &intra {
            next.entry(&e.from).or_default().insert(&e.to);
        }
        let reach = |from: &str| {
            let mut seen = BTreeSet::new();
            let mut todo = vec![from];
            while let Some(n) = todo.pop() {
                for &m in next.get(n).into_iter().flatten() {
                    if seen.insert(m) {
                        todo.push(m);
                    }
                }
            }
            seen
        };
        let reaches: BTreeMap<&str, BTreeSet<&str>> = next.keys().map(|&n| (n, reach(n))).collect();
        let mut found = BTreeSet::new();
        for (&n, r) in &reaches {
            let group: Vec<&str> =
                r.iter().copied().filter(|m| reaches.get(m).is_some_and(|back| back.contains(n))).collect();
            if group.len() > 2 {
                found.insert(group.join(", "));
            }
        }
        if !found.is_empty() {
            out.insert(k.name.clone(), found);
        }
    }
    out
}

fn members(cycle: &str) -> BTreeSet<&str> {
    cycle.split(", ").collect()
}

/// Found cycles against listed ones, for one crate.
fn cycle_problems(krate: &str, found: &BTreeSet<String>, listed: &[String], bad: &mut Vec<String>) {
    let mut accounted: BTreeSet<&str> = BTreeSet::new();
    for c in found.iter().filter(|c| !listed.contains(c)) {
        let m = members(c);
        let inside: Vec<&String> = listed.iter().filter(|l| members(l).is_subset(&m)).collect();
        if let Some(was) = listed.iter().find(|l| m.is_subset(&members(l))) {
            accounted.insert(was);
            bad.push(format!(
                "{RATCHETS}: [cycles] {krate}: \"{was}\" shrank to \"{c}\"; replace it with the smaller cycle"
            ));
        } else if inside.is_empty() {
            bad.push(format!(
                "{krate}: new cycle \"{c}\"; cut an edge (`construct-dev code-graph` shows every reference), or if it is intended add it under [cycles] \"{krate}\" in {RATCHETS}"
            ));
        } else {
            accounted.extend(inside.iter().map(|l| l.as_str()));
            let was: Vec<String> = inside.iter().map(|l| format!("\"{l}\"")).collect();
            let old: BTreeSet<&str> = inside.iter().flat_map(|l| members(l)).collect();
            let new: Vec<&str> = m.difference(&old).copied().collect();
            let joined = if new.is_empty() { String::new() } else { format!(", now with {}", new.join(", ")) };
            bad.push(format!(
                "{krate}: cycle {} grew to \"{c}\"{joined}; cut the edge that closed it (`construct-dev code-graph` shows every reference), or if it is intended replace it under [cycles] \"{krate}\" in {RATCHETS}",
                was.join(" and ")
            ));
        }
    }
    for l in listed.iter().filter(|l| !found.contains(*l) && !accounted.contains(l.as_str())) {
        bad.push(format!("{RATCHETS}: [cycles] {krate}: \"{l}\" is no longer a cycle; delete it from the list"));
    }
}

/// Non-test lines of every file over the soft limit, by repo-relative path.
fn long_files(root: &Path) -> BTreeMap<String, usize> {
    let mut out = BTreeMap::new();
    for f in crate::scratch::rust_sources(&root.join("crates")) {
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

    let real = cycles(root);
    let empty = BTreeSet::new();
    for krate in real.keys().chain(r.cycles.keys()).collect::<BTreeSet<_>>() {
        let listed = r.cycles.get(krate).map_or(&[][..], Vec::as_slice);
        cycle_problems(krate, real.get(krate).unwrap_or(&empty), listed, &mut bad);
    }

    bad
}
