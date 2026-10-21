//! Bakes the git commit into the binary so every session log and
//! `construct manifest` can say exactly what produced a result.

use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn main() {
    let sha = git(&["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".into());
    // Dirty means the build differs from the commit in this crate's sources.
    let dirty = git(&["status", "--porcelain", "--", "."]).is_some_and(|s| !s.is_empty());
    println!("cargo:rustc-env=CONSTRUCT_GIT_SHA={sha}");
    println!("cargo:rustc-env=CONSTRUCT_GIT_DIRTY={dirty}");
    // Rerun when the sources change (dirty flag) or HEAD moves (sha).
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=Cargo.toml");
    if let Some(dir) = git(&["rev-parse", "--git-dir"]) {
        println!("cargo:rerun-if-changed={dir}/HEAD");
        if let Some(r) = git(&["symbolic-ref", "-q", "HEAD"]) {
            println!("cargo:rerun-if-changed={dir}/{r}");
        }
    }
    if let Some(common) = git(&["rev-parse", "--git-common-dir"]) {
        println!("cargo:rerun-if-changed={common}/packed-refs");
        if let Some(r) = git(&["symbolic-ref", "-q", "HEAD"]) {
            println!("cargo:rerun-if-changed={common}/{r}");
        }
    }
}
