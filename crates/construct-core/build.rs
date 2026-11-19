//! Bakes the git commit into the binary so every journal head and
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
    // A rerun-if-changed path that does not exist makes cargo rebuild on every
    // invocation, and a rebuilt `construct` relinks every dependent test
    // binary. `--git-path` resolves worktrees and packed refs; a path that is
    // not there (a packed ref has no file) is skipped, and logs/HEAD catches a
    // commit onto a ref that was packed.
    let mut watch = vec!["HEAD".to_string(), "logs/HEAD".to_string(), "packed-refs".to_string()];
    if let Some(r) = git(&["symbolic-ref", "-q", "HEAD"]) {
        watch.push(r);
    }
    for name in watch {
        if let Some(path) = git(&["rev-parse", "--git-path", &name])
            && std::path::Path::new(&path).exists()
        {
            println!("cargo:rerun-if-changed={path}");
        }
    }
}
