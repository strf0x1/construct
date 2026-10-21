//! `cargo xtask …`: the repo's one verdict command (alias in .cargo/config.toml).
//!
//! `check` runs each gate in order and stops at the first failure, printing one
//! line per gate and a last line of `ok` or `FAILED <gate>`, so an agent never
//! has to read cargo's output to know whether it is done. A failing gate's
//! output is printed above that last line.

use anyhow::{Context, Result};
use std::path::PathBuf;
use std::process::Command;
use std::time::Instant;

const GATES: &[(&str, &[&str])] = &[
    ("fmt", &["fmt", "--all", "--check"]),
    ("clippy", &["clippy", "--workspace", "--all-targets", "--", "-D", "warnings"]),
    ("test", &["test", "--workspace"]),
];

fn cargo() -> Command {
    Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
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
        let out = cargo().args(*args).current_dir(&root).output().with_context(|| format!("running cargo {name}"))?;
        let secs = t.elapsed().as_secs_f32();
        if !out.status.success() {
            println!("{name:<8} FAILED ({secs:.1}s)");
            print!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
            println!("FAILED {name}");
            return Ok(false);
        }
        println!("{name:<8} ok ({secs:.1}s)");
    }
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
