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
//! the diff can reach, with the end-to-end tests left to CI. `check
//! --affected` is CI on a pull request: as quick, plus the end-to-end test
//! files the diff reaches (ADR 0053); a push to main runs everything.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

mod affected;
mod quick;

pub use quick::Mode;

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

/// Returns true if every gate passed. A gate named in `skip` does not run.
pub fn check(mode: Mode, skip: &[String]) -> Result<bool> {
    let root = workspace_root()?;
    use crate::feature_map as map;
    let git = map::Git {
        is_commit: &|sha| map::is_commit(&root, sha),
        object: &|rev, path| map::object(&root, rev, path),
        changed_since: &|sha, path| map::changed_since(&root, sha, path),
    };
    let in_process: [(&str, &dyn Fn() -> Vec<String>); 5] = [
        ("docs", &|| [conventions(&root), crate::deps::unshared(&root)].concat()),
        ("map", &|| crate::feature_map::problems(&root, &git)),
        ("contract", &|| crate::contract::problems(&root)),
        ("arch", &|| crate::arch::problems(&root)),
        ("affected", &|| affected::problems(&root)),
    ];
    for (name, gate) in in_process.into_iter().filter(|(n, _)| !skip.iter().any(|s| s == n)) {
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
    let crates = quick::workspace_crates(&root);
    let changed = if mode == Mode::Full { None } else { quick::changed_paths(&root) };
    let reach = changed.as_deref().map_or_else(quick::everything, quick::touched);
    let it = match &changed {
        Some(paths) if mode == Mode::Affected => affected::reach(&root, paths),
        _ => affected::Reach::All("no diff to narrow by".into()),
    };
    if mode == Mode::Affected {
        match &it {
            affected::Reach::All(why) => println!("affected: every e2e test ({why})"),
            affected::Reach::Some(m) => {
                let total = affected::it_modules(&root).len();
                let list = m.iter().map(String::as_str).collect::<Vec<_>>().join(" ");
                println!("affected: {} of {total} e2e test files: {list}", m.len());
            }
        }
    }
    let plan = quick::plan(mode, &reach, &crates, &it);
    for (name, args) in plan.iter().filter(|(n, _)| !skip.iter().any(|s| s == n)) {
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
    if mode == Mode::Quick {
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

    pub(super) fn tree(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("xtask-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for d in ["docs/adr", "crates/a/src/tui"] {
            std::fs::create_dir_all(dir.join(d)).unwrap();
        }
        dir
    }

    pub(super) fn write(root: &Path, path: &str, text: &str) {
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
    fn a_clean_tree_has_no_complaints() {
        let root = tree("good");
        write(&root, "docs/adr/0001-a.md", "# a\n");
        write(&root, "docs/adr/0002-b.md", "# b\n");
        write(&root, "crates/a/src/lib.rs", "//! ok\n");
        assert_eq!(conventions(&root), Vec::<String>::new());
        let _ = std::fs::remove_dir_all(&root);
    }
}
