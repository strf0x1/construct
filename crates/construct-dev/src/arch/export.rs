//! `construct-dev code-graph`: the arch gate's scan as JSON, for tools that
//! draw the code's coupling (`scripts/workmap/`). Every reference from one
//! top-level module to another, and from one crate to another, with its file,
//! line and the items it names.
//!
//! The one choice to know: a path that stops at a module (`use crate::jev;`)
//! names no item, so the file is scanned again for `jev::x` paths, and each
//! becomes an edge naming `x` at its own line; the bare `use` line is
//! dropped once such uses are found. That is what turns "learn uses jev"
//! into "learn/gate.rs:138 calls jev::ask". It stays token-level: a method
//! call on a value of jev's type is not seen.

use super::edges::{CrossEdge, Edge, Krate, Source, krates};
use super::lex::Tok;
use super::{graph, names};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// The whole scan of every crate under `root/crates`.
pub fn code_graph(root: &Path) -> Value {
    let ks = krates(root);
    let (idents, target_mods) = names(&ks);
    let mut crates = Vec::new();
    let mut cross_all = Vec::new();
    for k in &ks {
        let (intra, cross) = graph(k, &idents, &target_mods, &mut Vec::new());
        let (intra, cross) = expand(k, intra, cross);
        crates.push(json!({
            "name": k.name,
            "modules": k.srcs.iter().map(Source::module).collect::<BTreeSet<_>>(),
            "edges": intra.iter().map(|e| json!({"from": e.from, "to": e.to, "file": e.file, "line": e.line, "items": e.items})).collect::<Vec<_>>(),
        }));
        cross_all.extend(cross.iter().map(|e| {
            json!({"from": e.from, "to": e.to, "modules": e.modules, "file": e.file, "line": e.line, "items": e.items})
        }));
    }
    json!({"crates": crates, "cross": cross_all})
}

/// Replace each bare module import with the `module::item` uses in its file.
fn expand(k: &Krate, intra: Vec<Edge>, cross: Vec<CrossEdge>) -> (Vec<Edge>, Vec<CrossEdge>) {
    let by_file: BTreeMap<&str, &Source> = k.srcs.iter().map(|s| (s.file.as_str(), s)).collect();
    let mut out_intra = Vec::new();
    for e in intra {
        let found = match (e.items.is_empty(), by_file.get(e.file.as_str())) {
            (true, Some(s)) => uses(s, &e.to),
            _ => Vec::new(),
        };
        if found.is_empty() {
            out_intra.push(e);
            continue;
        }
        out_intra.extend(found.into_iter().map(|(item, line)| Edge { line, items: vec![item], ..e.clone() }));
    }
    let mut out_cross = Vec::new();
    for e in cross {
        let bare: Vec<&String> = e.items.iter().filter(|i| !i.contains("::")).collect();
        let src = by_file.get(e.file.as_str());
        let mut found = Vec::new();
        for m in &bare {
            if let Some(s) = src {
                found.extend(uses(s, m).into_iter().map(|(item, line)| (format!("{m}::{item}"), line)));
            }
        }
        if found.is_empty() {
            out_cross.push(e);
            continue;
        }
        if bare.len() < e.items.len() {
            let items = e.items.iter().filter(|i| i.contains("::")).cloned().collect();
            out_cross.push(CrossEdge { items, ..e.clone() });
        }
        out_cross.extend(found.into_iter().map(|(item, line)| CrossEdge { line, items: vec![item], ..e.clone() }));
    }
    (out_intra, out_cross)
}

/// Every `module::item` path in `src` that starts at `module` (not after another `::`).
fn uses(src: &Source, module: &str) -> Vec<(String, usize)> {
    let t = &src.tokens;
    let mut out = Vec::new();
    for i in 0..t.len() {
        let starts = i == 0 || !matches!(t[i - 1].tok, Tok::PathSep);
        if !starts || !matches!(&t[i].tok, Tok::Ident(w) if w == module) {
            continue;
        }
        if let (Some(Tok::PathSep), Some(Tok::Ident(item))) =
            (t.get(i + 1).map(|x| &x.tok), t.get(i + 2).map(|x| &x.tok))
        {
            out.push((item.clone(), t[i].line));
        }
    }
    out
}
