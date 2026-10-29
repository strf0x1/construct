//! AGENTS.md convention 8, checked: a dependency more than one crate names is
//! pinned once in `[workspace.dependencies]` and taken with
//! `.workspace = true`, so two crates can't drift onto different versions.
//!
//! Part of the `docs` gate. It reads the manifests as TOML rather than asking
//! cargo, so it stays offline and takes milliseconds. Whether the dependency
//! is allowed at all (licenses, advisories, sources) is `cargo deny`'s job.

use std::collections::BTreeMap;
use std::path::Path;

const SECTIONS: &[&str] = &["dependencies", "dev-dependencies", "build-dependencies"];

/// One line per dependency that several crates name and at least one of them
/// pins by itself.
pub(crate) fn unshared(root: &Path) -> Vec<String> {
    // name -> (crates naming it, crates not taking it from the workspace)
    let mut seen: BTreeMap<String, (Vec<String>, Vec<String>)> = BTreeMap::new();
    let mut crates: Vec<_> = std::fs::read_dir(root.join("crates")).into_iter().flatten().flatten().collect();
    crates.sort_by_key(|c| c.file_name());
    for dir in crates {
        let Ok(text) = std::fs::read_to_string(dir.path().join("Cargo.toml")) else { continue };
        let Ok(manifest) = text.parse::<toml::Table>() else { continue };
        let krate = dir.file_name().to_string_lossy().into_owned();
        for section in SECTIONS.iter().filter_map(|s| manifest.get(*s).and_then(|v| v.as_table())) {
            for (name, spec) in section {
                let from_workspace = spec.get("workspace").and_then(|w| w.as_bool()) == Some(true);
                let entry = seen.entry(name.clone()).or_default();
                if !entry.0.contains(&krate) {
                    entry.0.push(krate.clone());
                }
                if !from_workspace && !entry.1.contains(&krate) {
                    entry.1.push(krate.clone());
                }
            }
        }
    }
    seen.into_iter()
        .filter(|(_, (named, own))| named.len() > 1 && !own.is_empty())
        .map(|(name, (named, own))| {
            format!(
                "Cargo.toml: `{name}` is used by {} but {} pins it itself; \
                 put it in [workspace.dependencies] and use `{name}.workspace = true`",
                named.join(", "),
                own.join(", ")
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(name: &str, manifests: &[(&str, &str)]) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("deps-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for (krate, text) in manifests {
            std::fs::create_dir_all(root.join("crates").join(krate)).unwrap();
            std::fs::write(root.join("crates").join(krate).join("Cargo.toml"), text).unwrap();
        }
        root
    }

    #[test]
    fn a_dependency_two_crates_pin_themselves_is_reported() {
        let root = tree(
            "bad",
            &[
                ("a", "[dependencies]\ntoml = \"1\"\nserde.workspace = true\nonly-a = \"1\"\n"),
                ("b", "[dev-dependencies]\ntoml = { workspace = true }\nserde = { workspace = true }\n"),
            ],
        );
        let bad = unshared(&root);
        assert_eq!(bad.len(), 1, "{bad:?}");
        assert!(bad[0].contains("`toml` is used by a, b but a pins it itself"), "{bad:?}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_real_workspace_shares_its_dependencies() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        assert_eq!(unshared(&root), Vec::<String>::new());
    }
}
