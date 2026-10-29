//! The real module graph of `crates/construct/src`: every reference from one
//! top-level module to another, with the file and line it was found at.
//!
//! A file belongs to its top-level module (`tui/app.rs` and `tui/mod.rs` are
//! `tui`; `main.rs` is `main`, `lib.rs` is `lib`). An edge is any path that
//! names another top-level module: `crate::x`, each branch of a grouped
//! `crate::{x, y::{..}}`, `super::` chains that climb to the crate root
//! (counting inline `mod m { }` blocks), and in `main.rs`, which is its own
//! crate, `construct::x`. A module's references to itself are not edges, and
//! neither is test code (`#[cfg(test)]` items and `tests.rs` files): tests build fixtures from anywhere, and the table is
//! about what ships in the binary.

use super::lex::{Tok, Token, lex, strip_tests};
use std::path::Path;

pub(super) const SRC: &str = "crates/construct/src";

#[derive(Debug, Clone)]
pub(super) struct Edge {
    pub(super) from: String,
    pub(super) to: String,
    pub(super) file: String,
    pub(super) line: usize,
}

/// One source file of the crate, lexed, with its test code removed.
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

/// Every `.rs` file under `crates/construct/src`, in path order.
pub(super) fn sources(root: &Path) -> Vec<Source> {
    let src = root.join(SRC);
    let mut files = Vec::new();
    walk(&src, &mut files);
    files.sort();
    files
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
            let (tokens, _) =
                if last == "tests.rs" { Default::default() } else { strip_tests(lex(&text), text.lines().count()) };
            let file = f.strip_prefix(root).unwrap_or(&f).display().to_string();
            Some(Source { file, path, tokens })
        })
        .collect()
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
fn heads(t: &[Token], i: usize) -> Vec<(String, usize)> {
    if let Some(w) = ident(t, i) {
        return vec![(w.to_string(), t[i].line)];
    }
    if !is(t, i, &Tok::Punct('{')) {
        return Vec::new();
    }
    let (mut out, mut depth, mut start) = (Vec::new(), 0, true);
    for tok in &t[i..] {
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
            Tok::Ident(w) if start && depth == 1 => out.push((w.clone(), tok.line)),
            _ => {}
        }
        start = false;
    }
    out
}

/// The top-level modules `src` names at its crate root, with lines; names
/// that are not modules are the caller's to judge.
pub(super) fn references(src: &Source) -> Vec<(String, usize)> {
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
        let root = match ident(t, i) {
            Some("crate") => Some(i + 2),
            Some("construct") if src.path == ["main"] => Some(i + 2),
            Some("super") => {
                let mut j = i;
                while ident(t, j) == Some("super") && is(t, j + 1, &Tok::PathSep) {
                    j += 2;
                }
                // Climbing exactly to the crate root names a top-level module next.
                ((j - i) / 2 == src.path.len() + mods.len()).then_some(j)
            }
            _ => None,
        };
        if let Some(j) = root {
            out.extend(heads(t, j).into_iter().filter(|(w, _)| !matches!(w.as_str(), "self" | "super" | "crate")));
        }
    }
    out
}
