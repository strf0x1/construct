//! `cargo xtask …`: the repo's one verdict command (alias in .cargo/config.toml).
//!
//! `check` runs each gate in order and stops at the first failure, printing one
//! line per gate and a last line of `ok` or `FAILED <gate>`, so an agent never
//! has to read cargo's output to know whether it is done. A failing gate's
//! output is printed above that last line. A test failure caused by a golden
//! snapshot is reported as `FAILED snapshots`.

use anyhow::{Context, Result};
use std::path::PathBuf;
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

/// Returns true if every gate passed.
pub fn check() -> Result<bool> {
    let root = workspace_root()?;
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
