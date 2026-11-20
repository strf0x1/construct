//! A narrowed check's reach: what a diff touches, which workspace crates have
//! tests and which crates link them, and the plan. `check --quick` runs the
//! static gates plus the unit and contract tests of every crate the diff
//! reaches (a touched crate and every crate that depends on it), leaving the
//! end-to-end tests and golden snapshots to CI. `check --affected` is what CI
//! runs on a pull request: the same, plus the e2e test files the diff
//! reaches (`affected.rs`).

use super::GATES;
use super::affected::Reach;
use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

/// What a diff can reach, from the paths it touches.
#[derive(Default, Debug, PartialEq)]
pub(super) struct Touched {
    /// Directory names under `crates/` the diff touches.
    crates: BTreeSet<String>,
    bench: bool,
    /// Manifests, lockfile, cargo config: anything can have changed.
    workspace: bool,
}

/// The reach of a full check: every crate, the bench, the workspace.
pub(super) fn everything() -> Touched {
    Touched { bench: true, workspace: true, ..Touched::default() }
}

pub(super) fn touched(paths: &[String]) -> Touched {
    let mut t = Touched::default();
    for p in paths {
        match p.as_str() {
            p if p.starts_with("crates/") => {
                if let Some(rest) = p.strip_prefix("crates/")
                    && let Some(name) = rest.split('/').next().filter(|n| !n.is_empty())
                {
                    t.crates.insert(name.to_string());
                }
            }
            p if p.starts_with("bench/") => t.bench = true,
            p if p.starts_with(".cargo/") || p.starts_with(".github/") => t.workspace = true,
            "Cargo.toml" | "Cargo.lock" | "rust-toolchain.toml" => t.workspace = true,
            _ => {}
        }
    }
    t
}

/// Paths changed since the branch left main, committed or not, or `None` when
/// git cannot say (then `--quick` assumes everything changed).
pub(super) fn changed_paths(root: &Path) -> Option<Vec<String>> {
    let git = |args: &[&str]| {
        let out = Command::new("git").args(args).current_dir(root).output().ok().filter(|o| o.status.success())?;
        Some(String::from_utf8_lossy(&out.stdout).lines().map(String::from).collect::<Vec<_>>())
    };
    let base = ["origin/main", "main"].iter().find_map(|b| git(&["merge-base", "HEAD", b])?.into_iter().next())?;
    let mut paths = git(&["diff", "--name-only", &base])?;
    paths.extend(git(&["ls-files", "--others", "--exclude-standard"])?);
    Some(paths)
}

fn owned(args: &[&str]) -> Vec<String> {
    args.iter().map(|a| a.to_string()).collect()
}

/// A workspace crate under `crates/`: its directory, package name, whether
/// `cargo test -p` has anything to run, and the workspace packages it depends
/// on (normal or dev).
pub(super) struct Krate {
    dir: String,
    package: String,
    has_tests: bool,
    deps: Vec<String>,
}

/// The named dependencies of a manifest, normal and dev.
fn dep_names(manifest: &toml::Table) -> Vec<String> {
    ["dependencies", "dev-dependencies"]
        .iter()
        .filter_map(|k| manifest.get(*k).and_then(|d| d.as_table()))
        .flat_map(|d| d.keys().cloned())
        .collect()
}

/// Whether the crate has tests: a lib (unit tests), integration tests, or a
/// bin target that tests its own code. A binary-only crate whose bins all
/// set `test = false` (the construct composition root) has nothing to run.
fn has_tests(manifest: &toml::Table, dir: &Path) -> bool {
    if manifest.contains_key("lib") || manifest.contains_key("test") || dir.join("tests").is_dir() {
        return true;
    }
    match manifest.get("bin").and_then(|b| b.as_array()) {
        Some(bins) => bins.iter().any(|b| b.get("test").and_then(|t| t.as_bool()) != Some(false)),
        None => dir.join("src/main.rs").is_file(), // cargo's auto-detected bin tests by default
    }
}

/// Every crate under `crates/`, in directory order. A new crate is picked up
/// without a code change here; the quick gate runs its tests when it changes.
pub(super) fn workspace_crates(root: &Path) -> Vec<Krate> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(root.join("crates")) else { return out };
    for e in entries.flatten() {
        let dir = e.path();
        let Ok(text) = std::fs::read_to_string(dir.join("Cargo.toml")) else { continue };
        let Ok(manifest) = text.parse::<toml::Table>() else { continue };
        let Some(package) = manifest.get("package").and_then(|p| p.get("name")).and_then(|n| n.as_str()) else {
            continue;
        };
        out.push(Krate {
            dir: e.file_name().to_string_lossy().into_owned(),
            package: package.to_string(),
            has_tests: has_tests(&manifest, &dir),
            deps: dep_names(&manifest),
        });
    }
    let packages: BTreeSet<String> = out.iter().map(|k| k.package.clone()).collect();
    for k in &mut out {
        k.deps.retain(|d| packages.contains(d));
    }
    out.sort_by(|a, b| a.dir.cmp(&b.dir));
    out
}

/// The crates whose tests a diff reaches: those it touches and every crate
/// that depends on one of them, transitively. A change to the engine can
/// break the TUI's tests without touching the TUI.
fn reached<'a>(t: &Touched, crates: &'a [Krate]) -> BTreeSet<&'a str> {
    let mut hit: BTreeSet<&str> =
        crates.iter().filter(|k| t.workspace || t.crates.contains(&k.dir)).map(|k| k.package.as_str()).collect();
    loop {
        let more: Vec<&str> = crates
            .iter()
            .filter(|k| !hit.contains(k.package.as_str()) && k.deps.iter().any(|d| hit.contains(d.as_str())))
            .map(|k| k.package.as_str())
            .collect();
        if more.is_empty() {
            return hit;
        }
        hit.extend(more);
    }
}

/// How much of the check runs.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Mode {
    /// Every gate, every test.
    Full,
    /// The local loop: reached crates' unit and contract tests, no e2e.
    Quick,
    /// A pull request in CI: as `Quick`, plus the e2e test files reached.
    Affected,
}

/// The subprocess gates to run, in order. Full: all of `GATES`. Otherwise fmt
/// and clippy over the workspace, then the unit and contract tests of every
/// crate the diff reaches, then (`Affected` only) the e2e test files in `it`,
/// and the bench's own tests only if `bench/` changed. Binary-only crates are
/// skipped cleanly.
pub(super) fn plan(mode: Mode, t: &Touched, crates: &[Krate], it: &Reach) -> Vec<(String, Vec<String>)> {
    if mode == Mode::Full {
        return GATES.iter().map(|(n, a)| (n.to_string(), owned(a))).collect();
    }
    let mut gates = vec![];
    for (name, args) in GATES.iter().take(2) {
        gates.push((name.to_string(), owned(args)));
    }
    let hit = reached(t, crates);
    for k in crates.iter().filter(|k| k.has_tests && hit.contains(k.package.as_str())) {
        // construct-dev's tests/it.rs is end-to-end + snapshots: its own gate.
        let args = if k.package == "construct-dev" {
            owned(&["test", "-p", &k.package, "--lib", "--bins"])
        } else {
            owned(&["test", "-p", &k.package])
        };
        gates.push((format!("test-{}", k.package), args));
    }
    if mode == Mode::Affected {
        let mut args = owned(&["test", "-p", "construct-dev", "--test", "it"]);
        match it {
            Reach::All(_) => gates.push(("test-it".to_string(), args)),
            Reach::Some(m) if m.is_empty() => {}
            Reach::Some(m) => {
                args.push("--".to_string());
                args.extend(m.iter().map(|m| format!("{m}::")));
                gates.push(("test-it".to_string(), args));
            }
        }
    }
    if t.workspace || t.bench {
        gates.extend(GATES.iter().filter(|(n, _)| *n == "bench").map(|(n, a)| (n.to_string(), owned(a))));
    }
    gates
}

#[cfg(test)]
mod tests {
    use super::super::tests::{tree, write};
    use super::*;

    #[test]
    fn quick_runs_only_the_tests_the_diff_can_reach() {
        let krate = |name: &str, has_tests: bool, deps: &[&str]| Krate {
            dir: name.into(),
            package: name.into(),
            has_tests,
            deps: deps.iter().map(|d| d.to_string()).collect(),
        };
        let crates = vec![
            krate("construct", false, &["construct-core", "construct-tui"]),
            krate("construct-core", true, &[]),
            krate("construct-dev", true, &["construct-core"]),
            krate("construct-tui", true, &["construct-core"]),
        ];
        let all = Reach::All(String::new());
        let names = |t: &Touched| plan(Mode::Quick, t, &crates, &all).into_iter().map(|(n, _)| n).collect::<Vec<_>>();
        let paths = |p: &[&str]| touched(&p.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(names(&paths(&["docs/adr/0001-a.md", "AGENTS.md"])), ["fmt", "clippy"]);
        // The engine reaches every crate that links it.
        assert_eq!(
            names(&paths(&["crates/construct-core/src/tools/grep.rs"])),
            ["fmt", "clippy", "test-construct-core", "test-construct-dev", "test-construct-tui"]
        );
        assert_eq!(names(&paths(&["crates/construct-tui/src/app.rs"])), ["fmt", "clippy", "test-construct-tui"]);
        // The composition root is binary-only (test = false): nothing to run.
        assert_eq!(names(&paths(&["crates/construct/src/cli.rs"])), ["fmt", "clippy"]);
        assert_eq!(names(&paths(&["crates/construct-dev/tests/e2e.rs"])), ["fmt", "clippy", "test-construct-dev"]);
        assert_eq!(names(&paths(&["bench/bench.py"])), ["fmt", "clippy", "bench"]);
        let everything = ["fmt", "clippy", "test-construct-core", "test-construct-dev", "test-construct-tui", "bench"];
        assert_eq!(names(&paths(&["Cargo.lock"])), everything);
        assert_eq!(names(&paths(&[".github/workflows/ci.yml"])), everything);
        assert_eq!(
            plan(Mode::Full, &Touched::default(), &crates, &all).len(),
            GATES.len(),
            "a full check never narrows"
        );
    }

    #[test]
    fn affected_adds_the_reached_e2e_files_as_test_filters() {
        let crates =
            vec![Krate { dir: "construct-dev".into(), package: "construct-dev".into(), has_tests: true, deps: vec![] }];
        let t = touched(&["crates/construct-dev/src/xtask.rs".to_string()]);
        let it =
            |r: &Reach| plan(Mode::Affected, &t, &crates, r).into_iter().find(|(n, _)| n == "test-it").map(|(_, a)| a);
        let some = |m: &[&str]| Reach::Some(m.iter().map(|s| s.to_string()).collect());
        assert_eq!(it(&some(&[])), None, "nothing reached, no e2e run");
        let filtered = it(&some(&["jobs", "e2e"])).unwrap();
        assert_eq!(filtered[filtered.iter().position(|a| a == "--").unwrap()..], ["--", "e2e::", "jobs::"]);
        assert!(!it(&Reach::All("Cargo.lock".into())).unwrap().contains(&"--".to_string()));
        assert!(
            plan(Mode::Quick, &t, &crates, &some(&["jobs"])).iter().all(|(n, _)| n != "test-it"),
            "quick never runs e2e"
        );
    }

    #[test]
    fn crate_discovery_finds_tests_and_skips_binary_only_crates() {
        let root = tree("crates");
        for d in ["crates/engine", "crates/root", "crates/contracts/tests"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        write(&root, "crates/engine/Cargo.toml", "[package]\nname = \"engine\"\n\n[lib]\n");
        write(
            &root,
            "crates/root/Cargo.toml",
            "[package]\nname = \"root\"\n\n[[bin]]\nname = \"root\"\ntest = false\n",
        );
        write(&root, "crates/contracts/Cargo.toml", "[package]\nname = \"contracts\"\n");
        // crates/a (from `tree`) has no manifest: not a crate, ignored.
        let ks = workspace_crates(&root);
        let testable: Vec<&str> = ks.iter().filter(|k| k.has_tests).map(|k| k.package.as_str()).collect();
        assert_eq!(testable, ["contracts", "engine"]);
        let _ = std::fs::remove_dir_all(&root);
    }
}
