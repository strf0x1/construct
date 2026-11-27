//! Shared filesystem and subprocess helpers for the `xtask` gates.
//!
//! `run` executes a gate's command and collects its verdict and output. The
//! bench gate runs with `TMPDIR` at a fresh directory that is removed
//! afterwards, and anything left in it fails the gate rather than piling up in
//! `/tmp`: the bench tests once leaked task fixtures until the tmpfs ran out of
//! inodes and every shell on the machine broke.
//!
//! `rust_sources` walks every crate's `src/` tree in deterministic order so
//! `xtask`, `arch`, and `feature_map` share one walker without gates depending
//! back on `xtask` (ADR 0062).

use anyhow::Result;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

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

/// Run `cmd`; returns whether it passed and its stdout and stderr. The bench
/// gate runs in a private `TMPDIR` and also fails, naming them, on leftovers.
pub fn run(name: &str, cmd: &mut Command) -> Result<(bool, String)> {
    if name != "bench" {
        let out = cmd.output()?;
        return Ok((out.status.success(), text(&out)));
    }
    let dir = std::env::temp_dir().join(format!("construct-xtask-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let out = cmd.env("TMPDIR", &dir).output();
    let mut left: Vec<String> =
        std::fs::read_dir(&dir)?.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    std::fs::remove_dir_all(&dir)?;
    let out = out?;
    let mut text = text(&out);
    if !left.is_empty() {
        left.sort();
        text += &format!("left {} entries in TMPDIR; remove them with addCleanup: {}\n", left.len(), left.join(" "));
    }
    Ok((out.status.success() && left.is_empty(), text))
}

fn text(out: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
}
