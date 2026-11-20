//! Bakes the git commit into the binary so every journal head and
//! `construct manifest` can say exactly what produced a result, and bakes
//! the optional config layer (#271): `CONSTRUCT_BAKE_CONFIG=<path>` compiles
//! that config.toml into the binary as the lowest-precedence layer.

use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Embed the bake file (or an empty layer) where `config::baked` includes it.
/// Validated here so a bad bake fails the build, not the first run on the
/// target machine. Policy bakes; secrets never do — warn on the one config
/// corner where a literal credential can hide (auth headers).
fn bake_config() {
    println!("cargo:rerun-if-env-changed=CONSTRUCT_BAKE_CONFIG");
    let out_dir = std::env::var("OUT_DIR").unwrap_or_else(|_| panic!("OUT_DIR not set"));
    let dest = std::path::Path::new(&out_dir).join("baked-config.toml");
    let text = match std::env::var("CONSTRUCT_BAKE_CONFIG") {
        Ok(path) if !path.is_empty() => {
            println!("cargo:rerun-if-changed={path}");
            let text =
                std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading CONSTRUCT_BAKE_CONFIG={path}: {e}"));
            let table: toml::Table =
                toml::from_str(&text).unwrap_or_else(|e| panic!("CONSTRUCT_BAKE_CONFIG={path} is not valid TOML: {e}"));
            for (name, provider) in table.get("providers").and_then(|v| v.as_table()).into_iter().flatten() {
                if has_auth_header(provider) {
                    println!("cargo:warning=baked providers.{name} has an auth header: bake policy, never secrets");
                }
            }
            if has_auth_header(&toml::Value::Table(table)) {
                println!("cargo:warning=baked [headers] has an auth header: bake policy, never secrets");
            }
            text
        }
        _ => String::new(),
    };
    std::fs::write(&dest, text).unwrap_or_else(|e| panic!("writing {}: {e}", dest.display()));
}

/// A header whose name looks like a credential (authorization, *-api-key…).
fn has_auth_header(value: &toml::Value) -> bool {
    value.get("headers").and_then(|v| v.as_table()).into_iter().flatten().any(|(name, _)| {
        let lower = name.to_ascii_lowercase();
        lower.contains("authorization") || lower.contains("api-key") || lower.contains("apikey")
    })
}

fn main() {
    bake_config();
    let sha = git(&["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".into());
    // Dirty means the build differs from the commit in the shipped crates'
    // sources: this engine, the TUI front end, and the construct binary that
    // composes them.
    let dirty =
        git(&["status", "--porcelain", "--", ".", "../construct", "../construct-tui"]).is_some_and(|s| !s.is_empty());
    println!("cargo:rustc-env=CONSTRUCT_GIT_SHA={sha}");
    println!("cargo:rustc-env=CONSTRUCT_GIT_DIRTY={dirty}");
    // Rerun when the sources change (dirty flag) or HEAD moves (sha).
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-changed=../construct/src");
    println!("cargo:rerun-if-changed=../construct/Cargo.toml");
    println!("cargo:rerun-if-changed=../construct-tui/src");
    println!("cargo:rerun-if-changed=../construct-tui/Cargo.toml");
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
