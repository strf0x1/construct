//! `cargo xtask size`: the release binary's size against the baseline in
//! `docs/ratchets.toml` `[binary]`, so a dependency that bloats the binary is
//! noticed by the PR that adds it instead of by a user downloading it.
//!
//! Unlike the line counts this is a band, not an exact number: the toolchain
//! floats on `stable` and CI's linker is not the developer's, so the same
//! source differs by a little from run to run. Growth past the band fails and
//! tells the agent to stop and put the cost to the user before raising the
//! baseline; shrinkage past it fails too, asking for the lower baseline, so an
//! improvement is not left as headroom for the next regression. Not part of
//! `check`: a fat-LTO release build takes minutes, so CI runs it as its own job.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::process::Stdio;

/// `[binary]` in `docs/ratchets.toml`.
#[derive(Deserialize, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Baseline {
    /// Size of the stripped release `construct`, as CI builds it.
    bytes: u64,
    /// How far from `bytes`, either way, is still noise.
    tolerance_pct: u64,
}

const BINARY: &str = "construct";

fn mib(n: u64) -> String {
    format!("{:.1} MiB", n as f64 / (1024.0 * 1024.0))
}

/// `None` when `actual` is within the band, else what to do about it.
fn verdict(b: &Baseline, actual: u64) -> Option<String> {
    let band = b.bytes * b.tolerance_pct / 100;
    let ratchets = crate::arch::RATCHETS;
    if actual > b.bytes + band {
        let grew = actual - b.bytes;
        Some(format!(
            "the release binary is {} ({} bytes), {} bigger than the {} baseline in {ratchets} ({}% allowed). \
             That is usually a new dependency or feature: stop, find what grew it (`git diff main -- Cargo.lock`, \
             `cargo tree -p construct --depth 1`, or `cargo bloat --release --crates -p construct` if installed), \
             and tell the user what was added and what it costs before going on. Only once they agree, set \
             `bytes = {actual}` under [binary] in {ratchets}. CI's build is the one that counts: a local build \
             on another OS can differ by a little.",
            mib(actual),
            actual,
            mib(grew),
            mib(b.bytes),
            b.tolerance_pct,
        ))
    } else if actual + band < b.bytes {
        Some(format!(
            "the release binary shrank to {} ({actual} bytes) from the {} baseline in {ratchets}; \
             lower `bytes` under [binary] to {actual} so the gain stays locked in",
            mib(actual),
            mib(b.bytes),
        ))
    } else {
        None
    }
}

fn baseline(root: &Path) -> Result<Baseline> {
    let path = root.join(crate::arch::RATCHETS);
    let text = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let table: toml::Table = text.parse().with_context(|| format!("parsing {}", path.display()))?;
    let section = table.get("binary").with_context(|| format!("{} has no [binary] section", path.display()))?;
    section.clone().try_into().with_context(|| format!("[binary] in {}", path.display()))
}

/// Build the release binary and return where cargo put it. Cargo's own
/// progress goes to the terminal; its JSON messages are read for the path, so
/// a custom `CARGO_TARGET_DIR` needs no guessing.
fn build(root: &Path) -> Result<Option<PathBuf>> {
    let out = crate::xtask::cargo()
        .args(["build", "--release", "--locked", "-p", BINARY, "--message-format", "json-render-diagnostics"])
        .current_dir(root)
        .stderr(Stdio::inherit())
        .output()?;
    if !out.status.success() {
        return Ok(None);
    }
    Ok(String::from_utf8_lossy(&out.stdout).lines().filter_map(executable).next_back())
}

/// The path of the `construct` executable in one line of cargo's JSON output.
fn executable(line: &str) -> Option<PathBuf> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    if v["reason"] != "compiler-artifact" || v["target"]["name"] != BINARY {
        return None;
    }
    v["executable"].as_str().map(PathBuf::from)
}

/// Returns true if the binary is within the band.
pub fn size() -> Result<bool> {
    let root = crate::xtask::workspace_root()?;
    let _lock = crate::xtask::exclusive(&root)?;
    let b = baseline(&root)?;
    let Some(exe) = build(&root)? else {
        println!("FAILED size (the release build failed; its output is above)");
        return Ok(false);
    };
    let actual = std::fs::metadata(&exe).with_context(|| format!("measuring {}", exe.display()))?.len();
    let change = (actual as f64 - b.bytes as f64) / b.bytes as f64 * 100.0;
    println!(
        "size     {} ({actual} bytes), baseline {} ({change:+.1}%, ±{}% allowed)",
        mib(actual),
        mib(b.bytes),
        b.tolerance_pct
    );
    match verdict(&b, actual) {
        None => {
            println!("ok");
            Ok(true)
        }
        Some(why) => {
            println!("  {why}");
            println!("FAILED size");
            Ok(false)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const B: Baseline = Baseline { bytes: 10_000_000, tolerance_pct: 2 };

    #[test]
    fn inside_the_band_passes_either_way() {
        for n in [9_800_000, 9_900_000, 10_000_000, 10_200_000] {
            assert_eq!(verdict(&B, n), None, "{n}");
        }
    }

    #[test]
    fn growth_past_the_band_says_to_stop_and_ask_the_user() {
        let why = verdict(&B, 10_200_001).unwrap();
        assert!(why.contains("stop") && why.contains("tell the user"), "{why}");
        assert!(why.contains("`bytes = 10200001`"), "{why}");
    }

    #[test]
    fn shrinkage_past_the_band_asks_for_the_lower_baseline() {
        let why = verdict(&B, 9_799_999).unwrap();
        assert!(why.contains("lower `bytes` under [binary] to 9799999"), "{why}");
    }

    #[test]
    fn the_baseline_section_is_strict_and_the_artifact_line_is_found() {
        assert_eq!(
            toml::from_str::<Baseline>("bytes = 5\ntolerance_pct = 3\n").unwrap(),
            Baseline { bytes: 5, tolerance_pct: 3 }
        );
        assert!(toml::from_str::<Baseline>("bytes = 5\ntolerance_pct = 3\nextra = 1\n").is_err());
        let line =
            r#"{"reason":"compiler-artifact","target":{"name":"construct"},"executable":"/t/release/construct"}"#;
        assert_eq!(executable(line), Some(PathBuf::from("/t/release/construct")));
        let lib = r#"{"reason":"compiler-artifact","target":{"name":"serde"},"executable":null}"#;
        assert_eq!(executable(lib), None);
    }
}
