//! The module half of the `arch` gate: one crate's top-level modules
//! against its `[crates.<pkg>.modules]` table. Every module needs an entry,
//! every entry needs a module, listed edges must still occur, and deny
//! rules need reasons and real targets. The crate half lives in `mod.rs`.

use super::{TABLE, uses_of};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};

use super::edges::{Edge, Krate, Source};

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(super) struct Entry {
    #[serde(default)]
    pub(super) may_use: Vec<String>,
    #[serde(default)]
    pub(super) must_not_use: Vec<Deny>,
    pub(super) used_only_by: Option<OnlyBy>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Deny {
    module: String,
    #[serde(default)]
    reason: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct OnlyBy {
    modules: Vec<String>,
    #[serde(default)]
    reason: String,
}

fn quoted(names: &BTreeSet<&str>) -> String {
    names.iter().map(|m| format!("\"{m}\"")).collect::<Vec<_>>().join(", ")
}

/// Every module needs an entry; every entry needs a module; listed edges
/// must still occur; deny rules need reasons and real targets.
pub(super) fn entries(k: &Krate, edges: &[Edge], modules: &BTreeMap<String, Entry>, bad: &mut Vec<String>) {
    let present: BTreeSet<&str> = k.srcs.iter().map(Source::module).collect();
    let uses = uses_of(edges);
    for m in &present {
        if !modules.contains_key(*m) {
            let file = k.srcs.iter().find(|s| s.module() == *m).map_or(k.src.as_str(), |s| s.file.as_str());
            let now = uses.get(m).map(quoted).unwrap_or_default();
            bad.push(format!(
                "{file}: module `{m}` has no entry in {TABLE}; add `[crates.{}.modules.{m}]` with `may_use = [{now}]`",
                k.name
            ));
        }
    }
    for (m, e) in modules {
        let key = format!("crates.{}.modules.{m}", k.name);
        if !present.contains(m.as_str()) {
            bad.push(format!(
                "{TABLE}: [{key}] names a module that no longer exists under {}; delete that entry",
                k.src
            ));
            continue;
        }
        let real = uses.get(m.as_str());
        for to in &e.may_use {
            if !real.is_some_and(|r| r.contains(to.as_str())) {
                bad.push(format!(
                    "{TABLE}: [{key}] may_use lists \"{to}\", but {m} no longer uses {to}; delete \"{to}\" from that list"
                ));
            }
        }
        for d in &e.must_not_use {
            if d.reason.trim().is_empty() {
                bad.push(format!(
                    "{TABLE}: [{key}] must_not_use \"{}\" has no reason; every deny rule says why it exists",
                    d.module
                ));
            }
            if !present.contains(d.module.as_str()) {
                bad.push(format!("{TABLE}: [{key}] must_not_use \"{}\" names no module; delete it", d.module));
            }
        }
        if let Some(o) = &e.used_only_by
            && o.reason.trim().is_empty()
        {
            bad.push(format!("{TABLE}: [{key}] used_only_by has no reason; every deny rule says why it exists"));
        }
    }
}

/// The module-level violations: a deny rule broken, a guarded module used by
/// an outsider, an edge the table does not list.
pub(super) fn edges(k: &Krate, edges: &[Edge], modules: &BTreeMap<String, Entry>, bad: &mut Vec<String>) {
    let empty = Entry::default();
    let mut reported = BTreeSet::new();
    for e in edges {
        let from = modules.get(&e.from).unwrap_or(&empty);
        let target = modules.get(&e.to).unwrap_or(&empty);
        let at = format!("{}:{}", e.file, e.line);
        if let Some(d) = from.must_not_use.iter().find(|d| d.module == e.to) {
            bad.push(format!("{at}: {} must not use {} ({TABLE}): {}", e.from, e.to, d.reason));
        } else if let Some(o) = target.used_only_by.as_ref().filter(|o| !o.modules.contains(&e.from)) {
            let only = o.modules.join(", ");
            bad.push(format!("{at}: only {only} may use {} ({TABLE}), not {}: {}", e.to, e.from, o.reason));
        } else if modules.contains_key(&e.from) && !from.may_use.contains(&e.to) && reported.insert((&e.from, &e.to)) {
            bad.push(format!(
                "{at}: {} uses {}, which {TABLE} does not list; add \"{}\" to may_use under [crates.{}.modules.{}] so the new dependency shows in the diff",
                e.from, e.to, e.to, k.name, e.from
            ));
        }
    }
}
