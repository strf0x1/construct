//! `cargo xtask …`: the repo's one verdict command (alias in .cargo/config.toml).
//!
//! `check` runs each gate in order and stops at the first failure, printing one
//! line per gate and a last line of `ok` or `FAILED <gate>`, so an agent never
//! has to read cargo's output to know whether it is done. A failing gate's
//! output is printed above that last line. A test failure caused by a golden
//! snapshot is reported as `FAILED snapshots`.

use anyhow::{Context, Result};
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

fn cargo() -> Command {
    Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
}

/// A gate's command: cargo with these args, or a program named by a leading `@`.
fn gate(args: &[&str]) -> Command {
    match args.split_first() {
        Some((first, rest)) if first.starts_with('@') => {
            let mut c = Command::new(&first[1..]);
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

/// Point git at the repo's hooks (`.githooks/post-checkout` seeds a new
/// worktree's `target/`) unless the user already chose a hooks path. Best effort.
fn enable_hooks(root: &PathBuf) {
    let git = |args: &[&str]| Command::new("git").args(args).current_dir(root).output().ok();
    let set = git(&["config", "--get", "core.hooksPath"]).is_some_and(|o| !o.stdout.is_empty());
    if !set && root.join(".githooks").is_dir() {
        git(&["config", "core.hooksPath", ".githooks"]);
    }
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
fn rust_sources(dir: &Path) -> Vec<PathBuf> {
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

/// Returns true if every gate passed.
pub fn check() -> Result<bool> {
    let root = workspace_root()?;
    enable_hooks(&root);
    let t = Instant::now();
    let problems = conventions(&root);
    let secs = t.elapsed().as_secs_f32();
    if !problems.is_empty() {
        println!("docs     FAILED ({secs:.1}s)");
        for p in &problems {
            println!("  {p}");
        }
        println!("FAILED docs");
        return Ok(false);
    }
    println!("docs     ok ({secs:.1}s)");
    for (name, args) in GATES {
        let t = Instant::now();
        let out = gate(args).current_dir(&root).output().with_context(|| format!("running gate {name}"))?;
        let secs = t.elapsed().as_secs_f32();
        if !out.status.success() {
            let output = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
            let name = if *name == "test" && output.contains("snapshot assertion for") { "snapshots" } else { name };
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
    println!("ok");
    Ok(true)
}

/// Re-record the golden snapshots and list the files that changed. Never edit
/// a `.snap` by hand; this is the way to accept an intended change.
pub fn snapshots() -> Result<bool> {
    let root = workspace_root()?;
    let ok = cargo()
        .args(["test", "-q", "-p", "construct-dev", "--test", "snapshots"])
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
    fn a_clean_tree_has_no_complaints() {
        let root = tree("good");
        write(&root, "docs/adr/0001-a.md", "# a\n");
        write(&root, "docs/adr/0002-b.md", "# b\n");
        write(&root, "crates/a/src/lib.rs", "//! ok\n");
        assert_eq!(conventions(&root), Vec::<String>::new());
        let _ = std::fs::remove_dir_all(&root);
    }
}
