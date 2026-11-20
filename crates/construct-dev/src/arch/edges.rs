//! The real module graph of every workspace crate (`crates/*/src`): every
//! reference from one top-level module to another, and every reference from
//! one crate to another, with the file and line it was found at.
//!
//! A file belongs to its top-level module (`tui/app.rs` and `tui/mod.rs` are
//! `tui`; `main.rs` is `main`, `lib.rs` is `lib`). A package's lib and bin
//! roots share one module pool: a module declared only in `main.rs`
//! (construct-dev's `arch`, `xtask`, …) is a top-level module like any other,
//! and `crate::x` inside it names that pool. An edge is any path that names
//! another top-level module: `crate::x`, each branch of a grouped
//! `crate::{x, y::{..}}`, `super::` chains that climb to the crate root
//! (counting inline `mod m { }` blocks), and the package's own lib name
//! (`construct_dev::x` in construct-dev). A path
//! rooted at *another* workspace crate's name is a cross-crate edge instead,
//! recorded crate-to-crate with the target crate's top-level modules the
//! path names (for module-scoped crate deny rules). A module's references
//! to itself are not edges,
//! and neither is test code (`#[cfg(test)]` items and `tests.rs` files):
//! tests build fixtures from anywhere, and the table is about what ships in
//! the binary.

use super::lex::{Tok, Token, lex, strip_tests};
use std::collections::BTreeMap;
use std::path::Path;

/// An edge between two top-level modules of one crate.
#[derive(Debug, Clone)]
pub(super) struct Edge {
    pub(super) from: String,
    pub(super) to: String,
    pub(super) file: String,
    pub(super) line: usize,
    /// The names right after the module (`crate::jev::ask` names `ask`);
    /// empty when the path stops at the module (`use crate::jev;`).
    pub(super) items: Vec<String>,
}

/// An edge from one workspace crate to another (package names), with the
/// target crate's top-level modules the path names (`construct_core::secrets`
/// names `secrets`; a path naming only items, like `construct_tui::run`,
/// names none). Module-scoped crate deny rules match against these.
#[derive(Debug, Clone)]
pub(super) struct CrossEdge {
    pub(super) from: String,
    pub(super) to: String,
    pub(super) modules: Vec<String>,
    pub(super) file: String,
    pub(super) line: usize,
    /// Each module path named, with the item after it when there is one:
    /// `jev::ask`, or `jev` alone.
    pub(super) items: Vec<String>,
}

/// What a path names: a module of the same package (with the items after
/// it), or another crate with the first segment of each branch after its
/// root (candidate modules) and the items after each.
pub(super) enum Ref {
    Module(String, Vec<String>),
    Crate(String, Vec<(String, Vec<String>)>),
}

/// One source file of a crate, lexed, with its test code removed.
pub(super) struct Source {
    /// Repo-relative path.
    pub(super) file: String,
    /// Module path inside the crate: `tui/app.rs` is `[tui, app]`, `tui/mod.rs` is `[tui]`.
    pub(super) path: Vec<String>,
    pub(super) tokens: Vec<Token>,
}

impl Source {
    pub(super) fn module(&self) -> &str {
        self.path.first().map_or("", String::as_str)
    }
}

/// One workspace crate: its package name, its `src` path, its lexed files.
pub(super) struct Krate {
    /// Package name from Cargo.toml (`construct-dev`); the crate ident is
    /// this with `-` turned into `_`.
    pub(super) name: String,
    /// Repo-relative source root, `crates/<dir>/src`.
    pub(super) src: String,
    pub(super) srcs: Vec<Source>,
}

/// Every crate under `crates/` with a `src/`, in path order.
pub(super) fn krates(root: &Path) -> Vec<Krate> {
    let mut dirs: Vec<std::path::PathBuf> = std::fs::read_dir(root.join("crates"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.join("src").is_dir())
        .collect();
    dirs.sort();
    dirs.into_iter()
        .map(|dir| {
            let name = package_name(&dir)
                .unwrap_or_else(|| dir.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned()));
            let src = dir.join("src");
            let mut files = Vec::new();
            walk(&src, &mut files);
            files.sort();
            let srcs = files
                .into_iter()
                .filter_map(|f| {
                    let text = std::fs::read_to_string(&f).ok()?;
                    let rel = f.strip_prefix(&src).ok()?;
                    let mut path: Vec<String> = rel.iter().map(|p| p.to_string_lossy().into_owned()).collect();
                    let last = path.pop()?;
                    let stem = last.strip_suffix(".rs")?;
                    if stem != "mod" {
                        path.push(stem.to_string());
                    }
                    // A `tests.rs` is the body of a `#[cfg(test)] mod tests;`: all test code.
                    let (tokens, _) = if last == "tests.rs" {
                        Default::default()
                    } else {
                        strip_tests(lex(&text), text.lines().count())
                    };
                    let file = f.strip_prefix(root).unwrap_or(&f).display().to_string();
                    Some(Source { file, path, tokens })
                })
                .collect();
            let src = src.strip_prefix(root).unwrap_or(&src).display().to_string();
            Krate { name, src, srcs }
        })
        .collect()
}

/// The `[package] name` of a crate directory, if its Cargo.toml says one.
fn package_name(dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(dir.join("Cargo.toml")).ok()?;
    let v: toml::Value = toml::from_str(&text).ok()?;
    Some(v.get("package")?.get("name")?.as_str()?.to_string())
}

pub(super) fn walk(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

fn ident(t: &[Token], i: usize) -> Option<&str> {
    match &t.get(i)?.tok {
        Tok::Ident(w) => Some(w),
        _ => None,
    }
}

fn is(t: &[Token], i: usize, tok: &Tok) -> bool {
    t.get(i).is_some_and(|x| x.tok == *tok)
}

/// The first segment of each path that starts at `i`: one name, or each
/// branch of a `{..}` group.
fn heads(t: &[Token], i: usize) -> Vec<(String, usize, usize)> {
    if let Some(w) = ident(t, i) {
        return vec![(w.to_string(), t[i].line, i)];
    }
    if !is(t, i, &Tok::Punct('{')) {
        return Vec::new();
    }
    let (mut out, mut depth, mut start) = (Vec::new(), 0, true);
    for (k, tok) in t.iter().enumerate().skip(i) {
        match &tok.tok {
            Tok::Punct('{') => {
                depth += 1;
                if depth == 1 {
                    continue;
                }
            }
            Tok::Punct('}') => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            Tok::Punct(',') if depth == 1 => {
                start = true;
                continue;
            }
            Tok::Ident(w) if start && depth == 1 => out.push((w.clone(), tok.line, k)),
            _ => {}
        }
        start = false;
    }
    out
}

/// The names after the segment at `i`, when a `::` follows it: `ask` in
/// `jev::ask`, `ask` and `Budget` in `jev::{ask, Budget}`.
fn after(t: &[Token], i: usize) -> Vec<String> {
    if !is(t, i + 1, &Tok::PathSep) {
        return Vec::new();
    }
    heads(t, i + 2).into_iter().map(|(w, ..)| w).filter(|w| w != "self").collect()
}

/// What `src` names at a crate root, with lines: its own top-level modules
/// (`crate::`, `super::` to the root, the package's own lib name) and other
/// workspace crates. `idents` maps crate ident → package name; `own` is this
/// package's ident. Names that are not modules are the caller's to judge.
pub(super) fn references(src: &Source, idents: &BTreeMap<String, String>, own: &str) -> Vec<(Ref, usize)> {
    let t = &src.tokens;
    let mut out = Vec::new();
    // Inline `mod m {` blocks deepen what `super` climbs from: (name, brace depth inside).
    let (mut mods, mut depth): (Vec<usize>, usize) = (Vec::new(), 0);
    for i in 0..t.len() {
        match t[i].tok {
            Tok::Punct('{') => {
                depth += 1;
                if ident(t, i.wrapping_sub(2)) == Some("mod") && ident(t, i.wrapping_sub(1)).is_some() {
                    mods.push(depth);
                }
            }
            Tok::Punct('}') => {
                if mods.last() == Some(&depth) {
                    mods.pop();
                }
                depth = depth.saturating_sub(1);
            }
            _ => {}
        }
        let after_sep = is(t, i.wrapping_sub(1), &Tok::PathSep) || is(t, i.wrapping_sub(1), &Tok::Punct('$'));
        if after_sep || !is(t, i + 1, &Tok::PathSep) {
            continue;
        }
        let Some(w) = ident(t, i) else { continue };
        // (is this a path into another crate, index of the first segment after the root)
        let root = match w {
            "crate" => Some((false, i + 2)),
            "super" => {
                let mut j = i;
                while ident(t, j) == Some("super") && is(t, j + 1, &Tok::PathSep) {
                    j += 2;
                }
                // Climbing exactly to the crate root names a top-level module next.
                ((j - i) / 2 == src.path.len() + mods.len()).then_some((false, j))
            }
            _ => idents.get(w).map(|_| (w != own, i + 2)),
        };
        match root {
            Some((false, j)) => {
                out.extend(
                    heads(t, j)
                        .into_iter()
                        .filter(|(w, ..)| !matches!(w.as_str(), "self" | "super" | "crate"))
                        .map(|(w, line, k)| (Ref::Module(w, after(t, k)), line)),
                );
            }
            Some((true, j)) => out.push((
                Ref::Crate(
                    idents[w].clone(),
                    heads(t, j)
                        .into_iter()
                        .filter(|(h, ..)| !matches!(h.as_str(), "self" | "super" | "crate"))
                        .map(|(h, _, k)| (h, after(t, k)))
                        .collect(),
                ),
                t[i].line,
            )),
            None => {}
        }
    }
    out
}
