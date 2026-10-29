//! The `arch` gate: `crates/construct/src`'s module graph checked against
//! `docs/architecture.toml`, plus the ratchets in `docs/ratchets.toml`.
//!
//! The table must describe the code as it is now, not as it once was: an
//! edge it does not list fails, and so does a listed edge that no longer
//! occurs, a module with no entry, an entry for a module that is gone, and a
//! deny rule without a reason. A new dependency is then a one-line edit to
//! the table that shows in the diff, never a silent `use`. Deny rules
//! (`must_not_use`, `used_only_by`) carry invariants AGENTS.md states in
//! prose. If the scanner finds no edges at all it fails, so a broken lexer
//! cannot pass by matching nothing. Edges come from `edges.rs` (which says
//! what counts), counts from `ratchet.rs`.

use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

mod edges;
mod lex;
mod ratchet;
#[cfg(test)]
mod tests;

use edges::{Edge, SRC, Source, references, sources};

const TABLE: &str = "docs/architecture.toml";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Table {
    modules: BTreeMap<String, Entry>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct Entry {
    #[serde(default)]
    may_use: Vec<String>,
    #[serde(default)]
    must_not_use: Vec<Deny>,
    used_only_by: Option<OnlyBy>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Deny {
    module: String,
    #[serde(default)]
    reason: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OnlyBy {
    modules: Vec<String>,
    #[serde(default)]
    reason: String,
}

/// Every problem with the module graph and the ratchets, one line each.
pub fn problems(root: &Path) -> Vec<String> {
    let srcs = sources(root);
    let mut bad = boundaries(root, &srcs);
    bad.extend(ratchet::problems(root));
    bad
}

/// Every reference from one top-level module to another, in file order.
fn graph(srcs: &[Source], bad: &mut Vec<String>) -> Vec<Edge> {
    let modules: BTreeSet<&str> = srcs.iter().map(Source::module).collect();
    let mut out = Vec::new();
    for s in srcs {
        for (to, line) in references(s) {
            if !modules.contains(to.as_str()) {
                bad.push(format!(
                    "{}:{line}: `{to}` at the crate root is not a module the arch gate knows; name the module it lives in",
                    s.file
                ));
            } else if to != s.module() {
                out.push(Edge { from: s.module().into(), to, file: s.file.clone(), line });
            }
        }
    }
    out
}

fn quoted(names: &BTreeSet<&str>) -> String {
    names.iter().map(|m| format!("\"{m}\"")).collect::<Vec<_>>().join(", ")
}

fn boundaries(root: &Path, srcs: &[Source]) -> Vec<String> {
    let mut bad = Vec::new();
    let table: Table = match std::fs::read_to_string(root.join(TABLE)) {
        Err(e) => return vec![format!("{TABLE}: cannot read it ({e})")],
        Ok(text) => match toml::from_str(&text) {
            Ok(t) => t,
            Err(e) => return vec![format!("{TABLE}: {}", e.message())],
        },
    };
    let edges = graph(srcs, &mut bad);
    if edges.is_empty() {
        bad.push(format!(
            "{SRC}: found no edge between modules; the arch scanner is broken (or the tree is empty), so nothing was checked"
        ));
    }
    let mut uses: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for e in &edges {
        uses.entry(&e.from).or_default().insert(&e.to);
    }
    let modules: BTreeSet<&str> = srcs.iter().map(Source::module).collect();
    let empty = Entry::default();

    for m in &modules {
        if !table.modules.contains_key(*m) {
            let file = srcs.iter().find(|s| s.module() == *m).map_or(SRC, |s| s.file.as_str());
            let now = uses.get(m).map(quoted).unwrap_or_default();
            bad.push(format!(
                "{file}: module `{m}` has no entry in {TABLE}; add `[modules.{m}]` with `may_use = [{now}]`"
            ));
        }
    }
    for (m, entry) in &table.modules {
        if !modules.contains(m.as_str()) {
            bad.push(format!(
                "{TABLE}: [modules.{m}] names a module that no longer exists under {SRC}; delete that entry"
            ));
            continue;
        }
        let real = uses.get(m.as_str());
        for to in &entry.may_use {
            if !real.is_some_and(|r| r.contains(to.as_str())) {
                bad.push(format!(
                    "{TABLE}: [modules.{m}] may_use lists \"{to}\", but {m} no longer uses {to}; delete \"{to}\" from that list"
                ));
            }
        }
        for d in &entry.must_not_use {
            if d.reason.trim().is_empty() {
                bad.push(format!(
                    "{TABLE}: [modules.{m}] must_not_use \"{}\" has no reason; every deny rule says why it exists",
                    d.module
                ));
            }
            if !modules.contains(d.module.as_str()) {
                bad.push(format!("{TABLE}: [modules.{m}] must_not_use \"{}\" names no module; delete it", d.module));
            }
        }
        if let Some(o) = &entry.used_only_by
            && o.reason.trim().is_empty()
        {
            bad.push(format!("{TABLE}: [modules.{m}] used_only_by has no reason; every deny rule says why it exists"));
        }
    }

    let mut reported = BTreeSet::new();
    for e in &edges {
        let entry = table.modules.get(&e.from).unwrap_or(&empty);
        let target = table.modules.get(&e.to).unwrap_or(&empty);
        let at = format!("{}:{}", e.file, e.line);
        if let Some(d) = entry.must_not_use.iter().find(|d| d.module == e.to) {
            bad.push(format!("{at}: {} must not use {} ({TABLE}): {}", e.from, e.to, d.reason));
        } else if let Some(o) = target.used_only_by.as_ref().filter(|o| !o.modules.contains(&e.from)) {
            let only = o.modules.join(", ");
            bad.push(format!("{at}: only {only} may use {} ({TABLE}), not {}: {}", e.to, e.from, o.reason));
        } else if table.modules.contains_key(&e.from)
            && !entry.may_use.contains(&e.to)
            && reported.insert((&e.from, &e.to))
        {
            bad.push(format!(
                "{at}: {} uses {}, which {TABLE} does not list; add \"{}\" to may_use under [modules.{}] so the new dependency shows in the diff",
                e.from, e.to, e.to, e.from
            ));
        }
    }
    bad
}
