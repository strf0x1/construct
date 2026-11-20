//! The `arch` gate: every workspace crate's module graph and the edges
//! between crates, checked against `docs/architecture.toml`, plus the
//! ratchets in `docs/ratchets.toml`.
//!
//! The table must describe the code as it is now, not as it once was: an
//! edge it does not list fails, and so does a listed edge that no longer
//! occurs, a module with no entry, an entry for a module that is gone, a
//! crate with no entry, an entry for a crate that is gone, and a deny rule
//! without a reason. A new dependency — module to module within a crate, or
//! crate to crate — is then a one-line edit to the table that shows in the
//! diff, never a silent `use`. Deny rules (`must_not_use`, `used_only_by`,
//! and their crate-level forms) carry invariants AGENTS.md states in prose.
//! If the scanner finds no edges at all it fails, so a broken lexer cannot
//! pass by matching nothing. Edges come from `edges.rs` (which says what
//! counts), the module checks from `modules.rs`, counts from `ratchet.rs`.
//! The schema is ADR 0047.

use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

mod edges;
mod export;
mod lex;
mod modules;
mod ratchet;
#[cfg(test)]
mod tests;

use edges::{CrossEdge, Edge, Krate, Ref, Source, krates, references};
pub use export::code_graph;

const TABLE: &str = "docs/architecture.toml";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Table {
    crates: BTreeMap<String, CrateEntry>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct CrateEntry {
    #[serde(default)]
    may_use_crates: Vec<String>,
    #[serde(default)]
    must_not_use_crates: Vec<CrateDeny>,
    used_only_by: Option<CrateOnlyBy>,
    #[serde(default)]
    modules: BTreeMap<String, modules::Entry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CrateDeny {
    #[serde(rename = "crate")]
    krate: String,
    /// Optional: ban only this top-level module of the target crate, not
    /// the whole crate (`{ crate = "construct-core", module = "secrets" }`).
    #[serde(default)]
    module: Option<String>,
    #[serde(default)]
    reason: String,
}

impl CrateDeny {
    /// How the rule's target is named in messages: `crate` or `crate::module`.
    fn target(&self) -> String {
        match &self.module {
            Some(m) => format!("{}::{m}", self.krate),
            None => self.krate.clone(),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CrateOnlyBy {
    crates: Vec<String>,
    #[serde(default)]
    reason: String,
}

/// Every problem with the module graphs, the crate edges and the ratchets,
/// one line each.
pub fn problems(root: &Path) -> Vec<String> {
    let ks = krates(root);
    let mut bad = boundaries(root, &ks);
    bad.extend(ratchet::problems(root));
    bad
}

/// Every reference from one top-level module to another of the same crate,
/// and from one crate to another, in file order.
fn graph(
    k: &Krate,
    idents: &BTreeMap<String, String>,
    target_mods: &BTreeMap<String, BTreeSet<String>>,
    bad: &mut Vec<String>,
) -> (Vec<Edge>, Vec<CrossEdge>) {
    let modules: BTreeSet<&str> = k.srcs.iter().map(Source::module).collect();
    let own = k.name.replace('-', "_");
    let (mut intra, mut cross) = (Vec::new(), Vec::new());
    for s in &k.srcs {
        for (r, line) in references(s, idents, &own) {
            match r {
                Ref::Module(to, items) => {
                    if !modules.contains(to.as_str()) {
                        bad.push(format!(
                            "{}:{line}: `{to}` at the crate root is not a module the arch gate knows; name the module it lives in",
                            s.file
                        ));
                    } else if to != s.module() {
                        intra.push(Edge { from: s.module().into(), to, file: s.file.clone(), line, items });
                    }
                }
                Ref::Crate(to, names) => {
                    // Only real top-level modules of the target count: a path
                    // naming an item (`construct_tui::run`) names no module.
                    let real: Vec<(String, Vec<String>)> = names
                        .into_iter()
                        .filter(|(n, _)| target_mods.get(&to).is_some_and(|m| m.contains(n)))
                        .collect();
                    let items = real
                        .iter()
                        .flat_map(|(m, its)| {
                            if its.is_empty() {
                                vec![m.clone()]
                            } else {
                                its.iter().map(|i| format!("{m}::{i}")).collect()
                            }
                        })
                        .collect();
                    let modules = real.into_iter().map(|(m, _)| m).collect();
                    cross.push(CrossEdge { from: k.name.clone(), to, modules, file: s.file.clone(), line, items });
                }
            }
        }
    }
    (intra, cross)
}

/// Crate ident → package name, and package name → its top-level modules.
type Names = (BTreeMap<String, String>, BTreeMap<String, BTreeSet<String>>);

fn names(ks: &[Krate]) -> Names {
    let idents = ks.iter().map(|k| (k.name.replace('-', "_"), k.name.clone())).collect();
    let mods = ks.iter().map(|k| (k.name.clone(), k.srcs.iter().map(|s| s.module().to_string()).collect())).collect();
    (idents, mods)
}

fn uses_of(edges: &[Edge]) -> BTreeMap<&str, BTreeSet<&str>> {
    let mut uses: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for e in edges {
        uses.entry(&e.from).or_default().insert(&e.to);
    }
    uses
}

fn boundaries(root: &Path, ks: &[Krate]) -> Vec<String> {
    let mut bad = Vec::new();
    let table: Table = match std::fs::read_to_string(root.join(TABLE)) {
        Err(e) => return vec![format!("{TABLE}: cannot read it ({e})")],
        Ok(text) => match toml::from_str(&text) {
            Ok(t) => t,
            Err(e) => return vec![format!("{TABLE}: {}", e.message())],
        },
    };
    let (idents, target_mods) = names(ks);
    let mut graphs = Vec::new();
    let mut cross: Vec<CrossEdge> = Vec::new();
    for k in ks {
        let (intra, xs) = graph(k, &idents, &target_mods, &mut bad);
        graphs.push((k, intra));
        cross.extend(xs);
    }
    if graphs.iter().all(|(_, edges)| edges.is_empty()) && cross.is_empty() {
        bad.push(
            "crates/*/src: found no edge between modules or crates; the arch scanner is broken (or the tree is empty), so nothing was checked"
                .to_string(),
        );
    }
    let empty = CrateEntry::default();
    for (k, edges) in &graphs {
        let entry = table.crates.get(&k.name).unwrap_or(&empty);
        if !table.crates.contains_key(&k.name) {
            bad.push(format!("{}: crate `{}` has no entry in {TABLE}; add `[crates.{}]`", k.src, k.name, k.name));
        }
        modules::entries(k, edges, &entry.modules, &mut bad);
        modules::edges(k, edges, &entry.modules, &mut bad);
    }
    crate_entries(&table, ks, &cross, &mut bad);
    crate_edges(&table, &cross, &mut bad);
    bad
}

/// Every crate needs an entry; every entry needs a crate; listed crate edges
/// must still occur; crate deny rules need reasons and real targets.
fn crate_entries(table: &Table, ks: &[Krate], cross: &[CrossEdge], bad: &mut Vec<String>) {
    let names: BTreeSet<&str> = ks.iter().map(|k| k.name.as_str()).collect();
    let mods: BTreeMap<&str, BTreeSet<&str>> =
        ks.iter().map(|k| (k.name.as_str(), k.srcs.iter().map(Source::module).collect())).collect();
    let mut uses: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for e in cross {
        uses.entry(&e.from).or_default().insert(&e.to);
    }
    for (name, entry) in &table.crates {
        if !names.contains(name.as_str()) {
            bad.push(format!(
                "{TABLE}: [crates.{name}] names a crate that no longer exists under crates/; delete that entry"
            ));
            continue;
        }
        let real = uses.get(name.as_str());
        for to in &entry.may_use_crates {
            if !real.is_some_and(|r| r.contains(to.as_str())) {
                bad.push(format!(
                    "{TABLE}: [crates.{name}] may_use_crates lists \"{to}\", but {name} no longer uses {to}; delete \"{to}\" from that list"
                ));
            }
        }
        for d in &entry.must_not_use_crates {
            if d.reason.trim().is_empty() {
                bad.push(format!(
                    "{TABLE}: [crates.{name}] must_not_use_crates \"{}\" has no reason; every deny rule says why it exists",
                    d.target()
                ));
            }
            if !names.contains(d.krate.as_str()) {
                bad.push(format!(
                    "{TABLE}: [crates.{name}] must_not_use_crates \"{}\" names no workspace crate; delete it",
                    d.krate
                ));
            } else if let Some(m) = &d.module
                && !mods.get(d.krate.as_str()).is_some_and(|ms| ms.contains(m.as_str()))
            {
                bad.push(format!(
                    "{TABLE}: [crates.{name}] must_not_use_crates \"{}\" module \"{m}\" names no module of {}; delete it",
                    d.krate, d.krate
                ));
            }
        }
        if let Some(o) = &entry.used_only_by
            && o.reason.trim().is_empty()
        {
            bad.push(format!(
                "{TABLE}: [crates.{name}] used_only_by has no reason; every deny rule says why it exists"
            ));
        }
    }
}

/// The crate-level violations, same shape as the module ones.
fn crate_edges(table: &Table, cross: &[CrossEdge], bad: &mut Vec<String>) {
    let empty = CrateEntry::default();
    let mut reported = BTreeSet::new();
    for e in cross {
        let from = table.crates.get(&e.from).unwrap_or(&empty);
        let target = table.crates.get(&e.to).unwrap_or(&empty);
        let at = format!("{}:{}", e.file, e.line);
        if let Some(d) = from
            .must_not_use_crates
            .iter()
            .find(|d| d.krate == e.to && d.module.as_ref().is_none_or(|m| e.modules.contains(m)))
        {
            bad.push(format!("{at}: {} must not use {} ({TABLE}): {}", e.from, d.target(), d.reason));
        } else if let Some(o) = target.used_only_by.as_ref().filter(|o| !o.crates.contains(&e.from)) {
            let only = o.crates.join(", ");
            bad.push(format!("{at}: only {only} may use {} ({TABLE}), not {}: {}", e.to, e.from, o.reason));
        } else if table.crates.contains_key(&e.from)
            && !from.may_use_crates.contains(&e.to)
            && reported.insert((&e.from, &e.to))
        {
            bad.push(format!(
                "{at}: crate {} uses crate {}, which {TABLE} does not list; add \"{}\" to may_use_crates under [crates.{}] so the new dependency shows in the diff",
                e.from, e.to, e.to, e.from
            ));
        }
    }
}
