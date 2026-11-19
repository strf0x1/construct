//! `cargo xtask …`: the repo's one verdict command (alias in .cargo/config.toml).
//!
//! `check` runs each gate in order and stops at the first failure, printing one
//! line per gate and a last line of `ok` or `FAILED <gate>`, so an agent never
//! has to read cargo's output to know whether it is done. A failing gate's
//! output is printed above that last line; a golden-snapshot failure reports
//! `FAILED snapshots`. `docs`, `map`, `contract` and `arch` read files in process.
//!
//! Builds in separate worktrees may run together. Local compiler and test
//! scheduling belongs to the user's Cargo frontend (mbx during the local
//! trial), rather than a repository-wide lock or fixed per-command limits.
//! `check --quick` is the inner loop: the static gates plus only the tests
//! the diff can reach, with the end-to-end tests left to CI.

use anyhow::{Context, Result};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

const GATES: &[(&str, &[&str])] = &[
    ("fmt", &["fmt", "--all", "--check"]),
    ("clippy", &["clippy", "--workspace", "--all-targets", "--", "-D", "warnings"]),
    ("test", &["test", "--workspace"]),
    // The bench's own machinery (tasks, scorers, compare); no model, no network.
    ("bench", &["@python3", "-m", "unittest", "discover", "-s", "bench", "-p", "test_*.py"]),
];

/// Resolve Cargo through PATH so the user's frontend also wraps nested gates.
/// Cargo's CARGO environment value names the underlying executable and can
/// bypass a frontend installed by mise.
fn cargo() -> Command {
    Command::new("cargo")
}

/// A gate's command: cargo with these args, or a program named by a leading `@`.
fn gate(args: &[String]) -> Command {
    match args.split_first().and_then(|(first, rest)| Some((first.strip_prefix('@')?, rest))) {
        Some((program, rest)) => {
            let mut c = Command::new(program);
            c.args(rest);
            c
        }
        _ => {
            let mut c = cargo();
            c.args(args);
            c
        }
    }
}

fn workspace_root() -> Result<PathBuf> {
    let out = cargo().args(["locate-project", "--workspace", "--message-format", "plain"]).output()?;
    anyhow::ensure!(out.status.success(), "cargo locate-project failed");
    let manifest = PathBuf::from(String::from_utf8(out.stdout)?.trim());
    Ok(manifest.parent().context("workspace manifest has no parent")?.to_path_buf())
}

/// Repo conventions the toolchain cannot express: ADRs are numbered `0001…N`
/// with no duplicate and no gap, and every source module opens with a `//!`
/// paragraph. Both were broken by a merge (two ADR 0008s, an undocumented
/// `learn/` split), so they are checked instead of remembered.
fn conventions(root: &Path) -> Vec<String> {
    let mut bad = Vec::new();

    let mut numbers: Vec<u32> = std::fs::read_dir(root.join("docs/adr"))
        .map(|d| {
            d.flatten()
                .filter_map(|e| {
                    let name = e.file_name();
                    name.to_str()?.split('-').next()?.parse().ok()
                })
                .collect()
        })
        .unwrap_or_default();
    numbers.sort_unstable();
    for n in numbers.windows(2).filter(|w| w[0] == w[1]).map(|w| w[0]) {
        bad.push(format!("docs/adr: two ADRs are numbered {n:04}"));
    }
    if let Some((i, n)) = numbers.iter().enumerate().find(|(i, n)| **n as usize != i + 1) {
        bad.push(format!("docs/adr: expected {:04} where {n:04} is (ADR {} of {})", i + 1, i + 1, numbers.len()));
    }

    // `autotests = false`: a test file nobody lists in `tests/it.rs` never runs.
    for it in
        std::fs::read_dir(root.join("crates")).into_iter().flatten().flatten().map(|c| c.path().join("tests/it.rs"))
    {
        let (Some(dir), Ok(listed)) = (it.parent(), std::fs::read_to_string(&it)) else { continue };
        let mut files: Vec<_> = std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()).collect();
        files.sort();
        for f in files.iter().filter(|f| f.extension().is_some_and(|x| x == "rs")) {
            let stem = f.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
            if stem != "it" && !listed.contains(&format!("mod {stem};")) {
                let rel = f.strip_prefix(root).unwrap_or(f).display();
                bad.push(format!("{rel}: not listed in tests/it.rs, so it would never run"));
            }
        }
    }

    for file in rust_sources(&root.join("crates")) {
        let opens_with_doc = std::fs::read_to_string(&file).is_ok_and(|s| s.starts_with("//!"));
        if !opens_with_doc {
            bad.push(format!(
                "{}: module does not open with `//!`",
                file.strip_prefix(root).unwrap_or(&file).display()
            ));
        }
    }
    bad
}

/// Every `.rs` file under `crates/*/src`, in path order.
pub(crate) fn rust_sources(dir: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
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
    let mut out = Vec::new();
    let Ok(crates) = std::fs::read_dir(dir) else { return out };
    for c in crates.flatten() {
        walk(&c.path().join("src"), &mut out);
    }
    out.sort();
    out
}

/// What a diff can reach, from the paths it touches.
#[derive(Default, Debug, PartialEq)]
struct Touched {
    /// Directory names under `crates/` the diff touches.
    crates: BTreeSet<String>,
    bench: bool,
    /// Manifests, lockfile, cargo config: anything can have changed.
    workspace: bool,
}

fn touched(paths: &[String]) -> Touched {
    let mut t = Touched::default();
    for p in paths {
        match p.as_str() {
            p if p.starts_with("crates/") => {
                if let Some(name) = p["crates/".len()..].split('/').next().filter(|n| !n.is_empty()) {
                    t.crates.insert(name.to_string());
                }
            }
            p if p.starts_with("bench/") => t.bench = true,
            p if p.starts_with(".cargo/") => t.workspace = true,
            "Cargo.toml" | "Cargo.lock" | "rust-toolchain.toml" => t.workspace = true,
            _ => {}
        }
    }
    t
}

/// Paths changed since the branch left main, committed or not, or `None` when
/// git cannot say (then `--quick` assumes everything changed).
fn changed_paths(root: &Path) -> Option<Vec<String>> {
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

/// A workspace crate under `crates/`: its directory, package name, and
/// whether `cargo test -p` has anything to run.
struct Krate {
    dir: String,
    package: String,
    has_tests: bool,
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
fn workspace_crates(root: &Path) -> Vec<Krate> {
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
        });
    }
    out.sort_by(|a, b| a.dir.cmp(&b.dir));
    out
}

/// The subprocess gates to run, in order. Full: all of `GATES`. Quick: fmt and
/// clippy, then the unit and contract tests of every crate the diff touches
/// (the end-to-end tests and golden snapshots are CI's), and the bench's own
/// tests only if `bench/` changed. Binary-only crates are skipped cleanly.
fn plan(quick: bool, t: &Touched, crates: &[Krate]) -> Vec<(String, Vec<String>)> {
    if !quick {
        return GATES.iter().map(|(n, a)| (n.to_string(), owned(a))).collect();
    }
    let mut gates = vec![];
    for (name, args) in GATES.iter().take(2) {
        gates.push((name.to_string(), owned(args)));
    }
    for k in crates.iter().filter(|k| k.has_tests) {
        if t.workspace || t.crates.contains(&k.dir) {
            // construct-dev's tests/it.rs is end-to-end + snapshots: CI's job.
            // Quick runs only its unit tests.
            let args = if k.package == "construct-dev" {
                owned(&["test", "-p", &k.package, "--lib", "--bins"])
            } else {
                owned(&["test", "-p", &k.package])
            };
            gates.push((format!("test-{}", k.package), args));
        }
    }
    if t.workspace || t.bench {
        gates.extend(GATES.iter().filter(|(n, _)| *n == "bench").map(|(n, a)| (n.to_string(), owned(a))));
    }
    gates
}

/// Returns true if every gate passed.
pub fn check(quick: bool) -> Result<bool> {
    let root = workspace_root()?;
    use crate::feature_map as map;
    let git = map::Git {
        is_commit: &|sha| map::is_commit(&root, sha),
        object: &|rev, path| map::object(&root, rev, path),
        changed_since: &|sha, path| map::changed_since(&root, sha, path),
    };
    let in_process: [(&str, &dyn Fn() -> Vec<String>); 4] = [
        ("docs", &|| [conventions(&root), crate::deps::unshared(&root)].concat()),
        ("map", &|| crate::feature_map::problems(&root, &git)),
        ("contract", &|| crate::contract::problems(&root)),
        ("arch", &|| crate::arch::problems(&root)),
    ];
    for (name, gate) in in_process {
        let t = Instant::now();
        let problems = gate();
        let secs = t.elapsed().as_secs_f32();
        if !problems.is_empty() {
            println!("{name:<8} FAILED ({secs:.1}s)");
            for p in &problems {
                println!("  {p}");
            }
            println!("FAILED {name}");
            return Ok(false);
        }
        println!("{name:<8} ok ({secs:.1}s)");
    }
    let crates = workspace_crates(&root);
    let everything = Touched { bench: true, workspace: true, ..Touched::default() };
    let reach = if quick { changed_paths(&root).map_or(everything, |p| touched(&p)) } else { everything };
    for (name, args) in &plan(quick, &reach, &crates) {
        let t = Instant::now();
        let (passed, output) =
            crate::scratch::run(name, gate(args).current_dir(&root)).with_context(|| format!("running gate {name}"))?;
        let secs = t.elapsed().as_secs_f32();
        if !passed {
            let name = if name.starts_with("test") && output.contains("snapshot assertion for") {
                "snapshots"
            } else {
                name.as_str()
            };
            println!("{name:<8} FAILED ({secs:.1}s)");
            print!("{output}");
            if name == "snapshots" {
                println!(
                    "read the diff above; if the change is intended, run `cargo xtask snapshots` and say so in the PR"
                );
            }
            println!("FAILED {name}");
            return Ok(false);
        }
        println!("{name:<8} ok ({secs:.1}s)");
    }
    if quick {
        println!(
            "quick: end-to-end tests and golden snapshots not run; CI runs them (`cargo test -p construct-dev --test it [filter]`)"
        );
    }
    println!("ok");
    Ok(true)
}

/// Re-record the golden snapshots and list the files that changed. Never edit
/// a `.snap` by hand; this is the way to accept an intended change.
pub fn snapshots() -> Result<bool> {
    let root = workspace_root()?;
    let ok = cargo()
        .args(["test", "-q", "-p", "construct-dev", "--test", "it", "--", "snapshots::"])
        .env("INSTA_UPDATE", "always")
        .current_dir(&root)
        .status()?
        .success();
    if !ok {
        println!("FAILED snapshots");
        return Ok(false);
    }
    let out = Command::new("git")
        .args(["status", "--short", "--", "crates/construct-dev/tests/snapshots"])
        .current_dir(&root)
        .output()?;
    let changed = String::from_utf8_lossy(&out.stdout);
    print!("{}", if changed.trim().is_empty() { "no snapshot changed\n".into() } else { changed });
    println!("ok");
    Ok(true)
}

/// Apply what can be applied mechanically: clippy's suggestions, then formatting.
pub fn fix() -> Result<bool> {
    let root = workspace_root()?;
    let clippy = ["clippy", "--fix", "--allow-dirty", "--allow-staged", "--workspace", "--all-targets"].as_slice();
    for args in [clippy, &["fmt", "--all"]] {
        if !cargo().args(args).current_dir(&root).status()?.success() {
            println!("FAILED {}", args[0]);
            return Ok(false);
        }
    }
    println!("ok");
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("xtask-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for d in ["docs/adr", "crates/a/src/tui"] {
            std::fs::create_dir_all(dir.join(d)).unwrap();
        }
        dir
    }

    fn write(root: &Path, path: &str, text: &str) {
        std::fs::write(root.join(path), text).unwrap();
    }

    #[test]
    fn a_duplicate_adr_number_and_an_undocumented_module_are_reported() {
        let root = tree("bad");
        write(&root, "docs/adr/0001-a.md", "# a\n");
        write(&root, "docs/adr/0001-b.md", "# b\n");
        write(&root, "docs/adr/template.md", "# t\n");
        write(&root, "crates/a/src/lib.rs", "//! ok\n");
        write(&root, "crates/a/src/tui/view.rs", "use std::fmt;\n");
        let bad = conventions(&root);
        assert!(bad.iter().any(|p| p.contains("two ADRs are numbered 0001")), "{bad:?}");
        assert!(bad.iter().any(|p| p.contains("expected 0002 where 0001 is (ADR 2 of 2)")), "{bad:?}");
        assert!(bad.iter().any(|p| p.contains("crates/a/src/tui/view.rs")), "{bad:?}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_test_file_missing_from_it_rs_is_reported() {
        let root = tree("untested");
        std::fs::create_dir_all(root.join("crates/a/tests")).unwrap();
        write(&root, "docs/adr/0001-a.md", "# a\n");
        write(&root, "crates/a/src/lib.rs", "//! ok\n");
        write(&root, "crates/a/tests/it.rs", "//! all\n#[path = \"one.rs\"]\nmod one;\n");
        write(&root, "crates/a/tests/one.rs", "//! one\n");
        write(&root, "crates/a/tests/two.rs", "//! two\n");
        let bad = conventions(&root);
        assert_eq!(bad.len(), 1, "{bad:?}");
        assert!(bad[0].contains("crates/a/tests/two.rs"), "{bad:?}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn quick_runs_only_the_tests_the_diff_can_reach() {
        let crates = vec![
            Krate { dir: "construct".into(), package: "construct".into(), has_tests: false },
            Krate { dir: "construct-core".into(), package: "construct-core".into(), has_tests: true },
            Krate { dir: "construct-dev".into(), package: "construct-dev".into(), has_tests: true },
        ];
        let names = |t: &Touched| plan(true, t, &crates).into_iter().map(|(n, _)| n).collect::<Vec<_>>();
        let paths = |p: &[&str]| touched(&p.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(names(&paths(&["docs/adr/0001-a.md", "AGENTS.md"])), ["fmt", "clippy"]);
        assert_eq!(
            names(&paths(&["crates/construct-core/src/tools/grep.rs"])),
            ["fmt", "clippy", "test-construct-core"]
        );
        // The composition root is binary-only (test = false): nothing to run.
        assert_eq!(names(&paths(&["crates/construct/src/cli.rs"])), ["fmt", "clippy"]);
        assert_eq!(names(&paths(&["crates/construct-dev/tests/e2e.rs"])), ["fmt", "clippy", "test-construct-dev"]);
        assert_eq!(names(&paths(&["bench/bench.py"])), ["fmt", "clippy", "bench"]);
        assert_eq!(
            names(&paths(&["Cargo.lock"])),
            ["fmt", "clippy", "test-construct-core", "test-construct-dev", "bench"]
        );
        assert_eq!(plan(false, &Touched::default(), &crates).len(), GATES.len(), "a full check never narrows");
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

    #[test]
    fn a_clean_tree_has_no_complaints() {
        let root = tree("good");
        write(&root, "docs/adr/0001-a.md", "# a\n");
        write(&root, "docs/adr/0002-b.md", "# b\n");
        write(&root, "crates/a/src/lib.rs", "//! ok\n");
        assert_eq!(conventions(&root), Vec::<String>::new());
        let _ = std::fs::remove_dir_all(&root);
    }
}
