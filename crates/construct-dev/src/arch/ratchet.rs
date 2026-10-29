//! Ratchets: counts of known debt in `docs/ratchets.toml` that must equal
//! the real count exactly. More fails because the debt grew; fewer fails too,
//! asking for the lower baseline, so an improvement is locked in by the PR
//! that made it.
//!
//! Two are kept. `[lines]`: each source file under `crates/*/src` over the
//! 300-line soft ceiling (AGENTS.md convention 3), counting only lines
//! outside `#[cfg(test)]` items, with `tests.rs` files left out. And
//! `wire_name_literals`: string literals in `crates/construct/src` outside
//! `tools/` that equal a tool's wire name (convention 5), with the names read
//! from the tools' `const NAME` declarations. Test code does not count for
//! either: a test that spells `"shell"` pins behaviour, it does not route on it.

use super::edges::{SRC, Source};
use super::lex::{Tok, lex, strip_tests};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub(super) const RATCHETS: &str = "docs/ratchets.toml";
pub(super) const SOFT_LIMIT: usize = 300;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Ratchets {
    wire_name_literals: usize,
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

/// Each literal outside `tools/` that equals a wire name, as `file:line "name"`.
fn wire_literals(srcs: &[Source]) -> (BTreeSet<String>, Vec<String>) {
    let tools = format!("{SRC}/tools/");
    let mut names = BTreeSet::new();
    for s in srcs.iter().filter(|s| s.file.starts_with(&tools)) {
        let t = &s.tokens;
        for i in 0..t.len() {
            let w = |k: usize, x: &str| matches!(&t.get(i + k).map(|t| &t.tok), Some(Tok::Ident(y)) if y == x);
            if w(0, "const") && w(1, "NAME") {
                // const NAME: &str = "…";
                if let Some(Tok::Str(name)) = t.get(i + 6).map(|t| &t.tok) {
                    names.insert(name.clone());
                }
            }
        }
    }
    let mut hits = Vec::new();
    for s in srcs.iter().filter(|s| !s.file.starts_with(&tools)) {
        for tok in &s.tokens {
            if let Tok::Str(x) = &tok.tok
                && names.contains(x)
            {
                hits.push(format!("{}:{} \"{x}\"", s.file, tok.line));
            }
        }
    }
    (names, hits)
}

pub(super) fn problems(root: &Path, srcs: &[Source]) -> Vec<String> {
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

    let (names, hits) = wire_literals(srcs);
    if names.is_empty() {
        bad.push(format!(
            "{SRC}/tools: found no `const NAME` wire names; the arch scanner is broken, so nothing was counted"
        ));
    }
    let (was, n) = (r.wire_name_literals, hits.len());
    if n > was {
        bad.push(format!(
            "{RATCHETS}: wire_name_literals grew from {was} to {n}: {}; use the tool's `NAME` const instead (AGENTS.md convention 5)",
            hits.join(", ")
        ));
    } else if n < was {
        bad.push(format!("{RATCHETS}: wire_name_literals dropped from {was} to {n}; lower the baseline to {n}"));
    }
    bad
}
