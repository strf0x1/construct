//! Which end-to-end test files a diff can reach, for `check --affected`
//! (ADR 0053). Every e2e test runs the real binary, which links every crate,
//! so the crate graph says "all of them" for nearly any change; and inside
//! construct-core the module graph is no narrower, since `config` names every
//! subsystem's settings and everything names `config`. So the narrowing is
//! declared, not derived: `crates/construct-dev/tests/affected.toml` maps
//! source areas to the test files that exercise them.
//!
//! The one design choice: unlisted means everything. A path under `crates/`
//! that no area claims reaches every e2e test, so a missing or stale area
//! costs CI time, never a skipped test. Only an area entry can narrow. A
//! full check on every push to main is the safety net for an area that
//! claims too little.

use serde::Deserialize;
use std::collections::BTreeSet;
use std::path::Path;

pub(super) const TABLE: &str = "crates/construct-dev/tests/affected.toml";
const IT: &str = "crates/construct-dev/tests/it.rs";
const TESTS: &str = "crates/construct-dev/tests/";

/// Outside `crates/`, the paths that change how every test builds or runs.
/// Any other path outside `crates/` (docs, bench, scripts) reaches no e2e
/// test unless an area names it: the tests read the repo only through
/// `common::root()`, and an area lists each file they read that way.
const EVERYTHING: &[&str] = &["Cargo.toml", "Cargo.lock", "rust-toolchain.toml", ".cargo/", ".github/", "clippy.toml"];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Table {
    area: Vec<Area>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Area {
    name: String,
    /// Repo-relative: a file, or a directory with a trailing `/`.
    paths: Vec<String>,
    /// Modules of `tests/it.rs`; empty means the area reaches no e2e test.
    tests: Vec<String>,
}

/// The e2e tests a diff reaches.
#[derive(Debug, PartialEq)]
pub(super) enum Reach {
    /// Every module, and the first path that forced it.
    All(String),
    /// These modules of `tests/it.rs` (possibly none).
    Some(BTreeSet<String>),
}

fn claims(area_path: &str, path: &str) -> bool {
    if area_path.ends_with('/') { path.starts_with(area_path) } else { path == area_path }
}

fn load(root: &Path) -> Result<Vec<Area>, String> {
    let text = std::fs::read_to_string(root.join(TABLE)).map_err(|e| format!("{TABLE}: {e}"))?;
    toml::from_str::<Table>(&text).map(|t| t.area).map_err(|e| format!("{TABLE}: {e}"))
}

/// The modules `tests/it.rs` declares.
pub(super) fn it_modules(root: &Path) -> BTreeSet<String> {
    std::fs::read_to_string(root.join(IT))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.trim().strip_prefix("mod ")?.strip_suffix(';').map(str::to_string))
        .filter(|m| m != "common")
        .collect()
}

/// What `paths` reach, given the table's areas and the modules of `it.rs`.
fn reach_with(paths: &[String], areas: &[Area], modules: &BTreeSet<String>) -> Reach {
    let mut out = BTreeSet::new();
    for p in paths {
        let claimed: Vec<&Area> = areas.iter().filter(|a| a.paths.iter().any(|ap| claims(ap, p))).collect();
        if !claimed.is_empty() {
            out.extend(claimed.iter().flat_map(|a| a.tests.iter().cloned()));
            continue;
        }
        let own = p.strip_prefix(TESTS).and_then(|f| f.strip_suffix(".rs")).filter(|m| modules.contains(*m));
        if let Some(m) = own {
            out.insert(m.to_string());
        } else if p.starts_with(&format!("{TESTS}snapshots/")) {
            out.insert("snapshots".to_string());
        } else if p.starts_with("crates/") || EVERYTHING.iter().any(|e| claims(e, p)) {
            return Reach::All(p.clone());
        }
    }
    Reach::Some(out)
}

/// What `paths` reach. A table that does not load reaches everything; the
/// `affected` gate reports why.
pub(super) fn reach(root: &Path, paths: &[String]) -> Reach {
    match load(root) {
        Ok(areas) => reach_with(paths, &areas, &it_modules(root)),
        Err(_) => Reach::All(TABLE.to_string()),
    }
}

/// The `affected` gate: the table loads, names no area twice, every path it
/// names exists (a directory with a trailing `/`), and every test it names is
/// a module of `tests/it.rs`.
pub fn problems(root: &Path) -> Vec<String> {
    let areas = match load(root) {
        Ok(a) => a,
        Err(e) => return vec![e],
    };
    let modules = it_modules(root);
    let mut bad = Vec::new();
    let mut seen = BTreeSet::new();
    for a in &areas {
        if !seen.insert(a.name.as_str()) {
            bad.push(format!("{TABLE}: two areas are named \"{}\"", a.name));
        }
        if a.paths.is_empty() {
            bad.push(format!("{TABLE}: area \"{}\" names no paths", a.name));
        }
        for p in &a.paths {
            let on_disk = root.join(p);
            let ok = if p.ends_with('/') { on_disk.is_dir() } else { on_disk.is_file() };
            if !ok {
                let kind = if p.ends_with('/') { "directory" } else { "file" };
                bad.push(format!("{TABLE}: area \"{}\" names {p}, which is no {kind}", a.name));
            }
        }
        for t in a.tests.iter().filter(|t| !modules.contains(*t)) {
            bad.push(format!("{TABLE}: area \"{}\" names test \"{t}\", which is not a module of {IT}", a.name));
        }
    }
    bad
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(name: &str, paths: &[&str], tests: &[&str]) -> Area {
        Area {
            name: name.into(),
            paths: paths.iter().map(|s| s.to_string()).collect(),
            tests: tests.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn set(xs: &[&str]) -> Reach {
        Reach::Some(xs.iter().map(|s| s.to_string()).collect())
    }

    #[test]
    fn only_a_listed_area_narrows_and_anything_else_under_crates_reaches_every_test() {
        let areas = [
            area("jobs", &["crates/construct-core/src/jobs/"], &["jobs", "e2e"]),
            area("tooling", &["crates/construct-dev/src/xtask/"], &[]),
            area("contract", &["CONTRACT.lock"], &["acp_payloads"]),
        ];
        let modules: BTreeSet<String> = ["jobs", "e2e", "acp_payloads", "snapshots"].map(String::from).into();
        let r = |p: &[&str]| reach_with(&p.iter().map(|s| s.to_string()).collect::<Vec<_>>(), &areas, &modules);

        assert_eq!(r(&["crates/construct-core/src/jobs/info.rs"]), set(&["e2e", "jobs"]));
        assert_eq!(r(&["crates/construct-dev/src/xtask/quick.rs"]), set(&[]));
        assert_eq!(r(&["crates/construct-dev/tests/jobs.rs"]), set(&["jobs"]));
        assert_eq!(r(&["crates/construct-dev/tests/snapshots/it__x.snap"]), set(&["snapshots"]));
        assert_eq!(r(&["docs/adr/0001-a.md", "bench/bench.py"]), set(&[]));
        assert_eq!(r(&["CONTRACT.lock"]), set(&["acp_payloads"]));
        // Unclaimed source, shared test helpers, it.rs itself, the workspace.
        for p in [
            "crates/construct-core/src/agent.rs",
            "crates/construct-dev/tests/common/mod.rs",
            "crates/construct-dev/tests/it.rs",
            "Cargo.lock",
            ".github/workflows/ci.yml",
        ] {
            assert_eq!(r(&["crates/construct-core/src/jobs/mod.rs", p]), Reach::All(p.into()), "{p}");
        }
        // A directory claim needs the slash: a file of the same stem is not inside it.
        assert_eq!(r(&["crates/construct-core/src/jobs.rs"]), Reach::All("crates/construct-core/src/jobs.rs".into()));
    }

    #[test]
    fn the_repo_table_narrows_leaf_areas_and_keeps_the_spine_whole() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).ancestors().nth(2).unwrap();
        assert_eq!(problems(root), Vec::<String>::new());
        let r = |p: &str| reach(root, &[p.to_string()]);
        let some = |p: &str| match r(p) {
            Reach::Some(m) => m,
            Reach::All(why) => panic!("{p} reached everything ({why})"),
        };
        // The slowest file, crash_sweep, is left out of a TUI or ACP change.
        for p in ["crates/construct-tui/src/view.rs", "crates/construct-core/src/acp/mod.rs"] {
            let m = some(p);
            assert!(!m.is_empty() && !m.contains("crash_sweep"), "{p}: {m:?}");
        }
        assert!(some("crates/construct-core/src/jobs/mod.rs").contains("crash_sweep"));
        assert!(some("crates/construct-dev/src/xtask.rs").is_empty());
        assert!(some("docs/adr/0001-a.md").is_empty());
        for p in
            ["crates/construct-core/src/agent.rs", "crates/construct-core/src/config.rs", "crates/construct/src/cli.rs"]
        {
            assert!(matches!(r(p), Reach::All(_)), "{p}");
        }
    }

    #[test]
    fn the_gate_names_missing_paths_unknown_tests_and_duplicate_areas() {
        let root = super::super::tests::tree("affected");
        for d in ["crates/construct-dev/tests", "crates/core/src/jobs"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        std::fs::write(root.join(IT), "mod common;\n#[path = \"jobs.rs\"]\nmod jobs;\n").unwrap();
        std::fs::write(
            root.join(TABLE),
            r#"
[[area]]
name = "jobs"
paths = ["crates/core/src/jobs/", "crates/core/src/gone.rs", "crates/core/src/jobs"]
tests = ["jobs", "nope"]

[[area]]
name = "jobs"
paths = ["crates/core/src/jobs/"]
tests = []
"#,
        )
        .unwrap();
        let bad = problems(&root);
        assert_eq!(bad.len(), 4, "{bad:#?}");
        assert!(bad.iter().any(|b| b.contains("gone.rs, which is no file")), "{bad:#?}");
        assert!(bad.iter().any(|b| b.contains("src/jobs, which is no file")), "{bad:#?}");
        assert!(bad.iter().any(|b| b.contains("\"nope\"")), "{bad:#?}");
        assert!(bad.iter().any(|b| b.contains("two areas are named \"jobs\"")), "{bad:#?}");
        let _ = std::fs::remove_dir_all(&root);
    }
}
