//! The baked config layer (#271): a config.toml compiled into the binary so a
//! dropped construct runs pre-configured with zero setup. `CONSTRUCT_BAKE_CONFIG`
//! at build time embeds the file (build.rs); this module lays the on-disk config
//! over it. Precedence is baked < file < env < flags, and every key remembers
//! which layer set it — `construct config` prints that provenance, because
//! invisible config in a dropped tool is a debugging nightmare. Policy bakes
//! (endpoint, model, sandbox, tools); secrets never do — keys resolve through
//! secretspec at runtime, so a copied binary carries no credential.

use super::Config;
use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::path::Path;

/// The baked layer; empty in an ordinary build (build.rs writes the file).
const BAKED: &str = include_str!(concat!(env!("OUT_DIR"), "/baked-config.toml"));

/// Which layer set a key: `"baked"`, `"file"` or `"env"`.
pub type Sources = BTreeMap<String, &'static str>;

/// Read the file layer (a missing file is the zero-config case) and merge it
/// over the baked one, recording where each leaf key came from.
pub fn load(path: &Path) -> Result<Config> {
    let file = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    parse_layers(BAKED, &file).with_context(|| format!("baked config + {}", path.display()))
}

/// Merge two TOML layers and deserialize the result. Split out from `load` so
/// the precedence rules are testable without a build-time bake or a filesystem.
fn parse_layers(baked: &str, file: &str) -> Result<Config> {
    let mut base: toml::Table = toml::from_str(baked).context("parsing the baked layer")?;
    let over: toml::Table = toml::from_str(file).context("parsing the file layer")?;
    let mut provenance = leaf_sources(&base, "", "baked");
    provenance.extend(leaf_sources(&over, "", "file"));
    let pinned = base.contains_key("context_window") || over.contains_key("context_window");
    merge(&mut base, over);
    let mut cfg: Config = toml::Value::Table(base).try_into().context("in the merged config")?;
    cfg.window_pinned = pinned;
    cfg.provenance = provenance;
    Ok(cfg)
}

/// One env override: the variable, the provenance key it sets, the setter.
type Override = (&'static str, &'static str, fn(&mut Config, String));

/// One-run overrides (`CONSTRUCT_*`), applied over both layers and recorded.
/// This table is the single source of truth for the env layer: what it sets
/// and what provenance reports come from the same rows.
pub fn apply_env(cfg: &mut Config) {
    const OVERRIDES: &[Override] = &[
        ("CONSTRUCT_BASE_URL", "base_url", |c, v| c.base_url = v),
        ("CONSTRUCT_MODEL", "model", |c, v| c.model = v),
        ("CONSTRUCT_SANDBOX", "sandbox.mode", |c, v| c.sandbox.mode = v),
        ("CONSTRUCT_ROUTER", "router.mode", |c, v| c.router.mode = v),
        ("CONSTRUCT_TOOLS_EDITOR", "tools.editor", |c, v| c.tools.editor = v),
        ("CONSTRUCT_TOOLS_WEB_SEARCH", "tools.web.search", |c, v| c.tools.web.search = v),
    ];
    for (var, key, set) in OVERRIDES {
        if let Ok(v) = std::env::var(var) {
            set(cfg, v);
            cfg.provenance.insert((*key).to_string(), "env");
        }
    }
}

/// Tables merge key by key down to the leaves; any other value (scalar,
/// array) is replaced whole by the upper layer — a file's `disabled` list
/// overrides the baked one, it does not extend it.
fn merge(base: &mut toml::Table, over: toml::Table) {
    for (key, value) in over {
        match (base.get_mut(&key), value) {
            (Some(toml::Value::Table(base_t)), toml::Value::Table(over_t)) => merge(base_t, over_t),
            (_, value) => {
                base.insert(key, value);
            }
        }
    }
}

/// Every leaf key of a layer, dotted (`sandbox.mode`), tagged with its source.
fn leaf_sources(table: &toml::Table, prefix: &str, source: &'static str) -> Sources {
    let mut out = Sources::new();
    for (key, value) in table {
        let path = if prefix.is_empty() { key.clone() } else { format!("{prefix}.{key}") };
        match value {
            toml::Value::Table(t) => out.extend(leaf_sources(t, &path, source)),
            _ => {
                out.insert(path, source);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_overrides_baked_key_by_key_and_tables_merge() {
        let cfg = parse_layers(
            "model = \"baked-model\"\nbase_url = \"https://baked.invalid/v1\"\n[sandbox]\nmode = \"workspace\"\n",
            "model = \"file-model\"\n[sandbox]\nallow_read = [\"/tmp\"]\n",
        )
        .unwrap();
        assert_eq!(cfg.model, "file-model");
        assert_eq!(cfg.base_url, "https://baked.invalid/v1");
        assert_eq!(cfg.sandbox.mode, "workspace");
        assert_eq!(cfg.provenance["model"], "file");
        assert_eq!(cfg.provenance["base_url"], "baked");
        assert_eq!(cfg.provenance["sandbox.mode"], "baked");
        assert_eq!(cfg.provenance["sandbox.allow_read"], "file");
    }

    #[test]
    fn an_empty_bake_is_an_ordinary_build() {
        let cfg = parse_layers("", "model = \"m\"\n").unwrap();
        assert_eq!(cfg.model, "m");
        assert_eq!(cfg.provenance["model"], "file");
        let empty = parse_layers("", "").unwrap();
        assert_eq!(empty.model, Config::default().model);
        assert!(empty.provenance.is_empty());
    }

    #[test]
    fn a_baked_window_pins_like_a_file_one() {
        assert!(parse_layers("context_window = 8000\n", "").unwrap().window_pinned);
        assert!(parse_layers("", "context_window = 8000\n").unwrap().window_pinned);
        assert!(!parse_layers("model = \"m\"\n", "").unwrap().window_pinned);
    }

    #[test]
    fn arrays_replace_whole_not_append() {
        let cfg = parse_layers("[tools]\ndisabled = [\"web\", \"mcp\"]\n", "[tools]\ndisabled = [\"web\"]\n").unwrap();
        assert_eq!(cfg.tools.disabled, vec!["web".to_string()]);
    }

    #[test]
    fn a_bad_layer_names_itself() {
        let err = parse_layers("model = ", "").unwrap_err();
        assert!(format!("{err:#}").contains("baked layer"), "{err:#}");
        let err = parse_layers("", "model = ").unwrap_err();
        assert!(format!("{err:#}").contains("file layer"), "{err:#}");
    }
}
