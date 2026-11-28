//! The baked config layer (#271): a config.toml compiled into the binary so a
//! dropped construct runs pre-configured with zero setup. `CONSTRUCT_BAKE_CONFIG`
//! at build time embeds the file (build.rs); this module lays the on-disk config
//! over it. A stamped binary (#363, `stamp.rs`) adds its own layer between the
//! two. Precedence is baked < stamped < file < env < flags, and every key remembers
//! which layer set it — `construct config` prints that provenance, because
//! invisible config in a dropped tool is a debugging nightmare. Policy bakes
//! (endpoint, model, sandbox, tools); secrets never do — keys resolve through
//! secretspec at runtime, so a copied binary carries no credential.

use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::path::Path;

/// The baked layer; empty in an ordinary build (build.rs writes the file).
const BAKED: &str = include_str!(concat!(env!("OUT_DIR"), "/baked-config.toml"));

/// Which layer set a key: `"baked"`, `"stamped"`, `"file"` or `"env"`.
pub type Sources = BTreeMap<String, &'static str>;

/// The merged TOML layers before deserialization into `Config`.
/// Keeping this module in terms of TOML tables keeps it independent of
/// `Config` and its fields.
pub(super) struct Merged {
    pub(super) table: toml::Table,
    pub(super) window_pinned: bool,
    pub(super) provenance: Sources,
}

/// Read the file layer (a missing file is the zero-config case) and merge it
/// over the stamped and baked ones, recording where each leaf key came from.
pub(super) fn load(path: &Path, stamped: Option<&toml::Table>) -> Result<Merged> {
    let file = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let stamped = stamped.cloned().unwrap_or_default();
    parse_layers(BAKED, stamped, &file).with_context(|| format!("baked and stamped config + {}", path.display()))
}

/// Merge the TOML layers. Split out from `load` so the precedence rules are
/// testable without a build-time bake or a filesystem.
fn parse_layers(baked: &str, stamped: toml::Table, file: &str) -> Result<Merged> {
    let mut base: toml::Table = toml::from_str(baked).context("parsing the baked layer")?;
    let over: toml::Table = toml::from_str(file).context("parsing the file layer")?;
    let mut provenance = leaf_sources(&base, "", "baked");
    provenance.extend(leaf_sources(&stamped, "", "stamped"));
    provenance.extend(leaf_sources(&over, "", "file"));
    let window_pinned = [&base, &stamped, &over].iter().any(|t| t.contains_key("context_window"));
    merge(&mut base, stamped);
    merge(&mut base, over);
    Ok(Merged { table: base, window_pinned, provenance })
}

/// One-run env overrides (`CONSTRUCT_*`): environment variable name and the
/// dotted TOML key it sets. Single source of truth for both merging and
/// provenance.
const ENV_OVERRIDES: &[(&str, &str)] = &[
    ("CONSTRUCT_BASE_URL", "base_url"),
    ("CONSTRUCT_MODEL", "model"),
    ("CONSTRUCT_SANDBOX", "sandbox.mode"),
    ("CONSTRUCT_SANDBOX_MEMORY_MAX", "sandbox.memory_max"),
    ("CONSTRUCT_ROUTER", "router.mode"),
    ("CONSTRUCT_TOOLS_WEB_SEARCH", "tools.web.search"),
];

/// Apply the `CONSTRUCT_*` env layer over `merged` and record `"env"` provenance.
pub(super) fn apply_env(merged: &mut Merged) {
    apply_env_from(merged, |var| std::env::var(var).ok());
}

fn apply_env_from(merged: &mut Merged, mut lookup: impl FnMut(&str) -> Option<String>) {
    let mut env = toml::Table::new();
    for &(var, key) in ENV_OVERRIDES {
        if let Some(v) = lookup(var) {
            set_dotted(&mut env, key, toml::Value::String(v));
        }
    }
    if env.is_empty() {
        return;
    }
    merged.provenance.extend(leaf_sources(&env, "", "env"));
    merge(&mut merged.table, env);
}

/// Insert `value` at a dotted path (`"tools.web.search"`), creating any
/// intermediate tables along the way.
fn set_dotted(table: &mut toml::Table, dotted: &str, value: toml::Value) {
    match dotted.split_once('.') {
        None => {
            table.insert(dotted.to_string(), value);
        }
        Some((head, rest)) => {
            let entry = table.entry(head.to_string()).or_insert_with(|| toml::Value::Table(toml::Table::new()));
            if !entry.is_table() {
                *entry = toml::Value::Table(toml::Table::new());
            }
            if let toml::Value::Table(child) = entry {
                set_dotted(child, rest, value);
            }
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
    use crate::config::Config;

    fn into_config(m: Merged) -> Result<Config> {
        let mut cfg: Config = toml::Value::Table(m.table).try_into().context("in the merged config")?;
        cfg.window_pinned = m.window_pinned;
        cfg.provenance = m.provenance;
        Ok(cfg)
    }

    fn parse_two(baked: &str, file: &str) -> Result<Config> {
        into_config(parse_layers(baked, toml::Table::new(), file)?)
    }

    #[test]
    fn the_stamped_layer_sits_between_baked_and_file() {
        let stamped: toml::Table =
            toml::from_str("model = \"stamped-model\"\nllm = \"hub\"\n[sandbox]\nmode = \"read-only\"\n").unwrap();
        let cfg = into_config(
            parse_layers(
                "model = \"baked\"\nbase_url = \"https://baked.invalid/v1\"\n",
                stamped,
                "[sandbox]\nmode = \"off\"\n",
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(cfg.model, "stamped-model");
        assert_eq!(cfg.llm, crate::config::Llm::Hub);
        assert_eq!(cfg.sandbox.mode, "off");
        assert_eq!(cfg.provenance["model"], "stamped");
        assert_eq!(cfg.provenance["llm"], "stamped");
        assert_eq!(cfg.provenance["base_url"], "baked");
        assert_eq!(cfg.provenance["sandbox.mode"], "file");
    }

    #[test]
    fn env_layer_merges_dotted_keys_without_clobbering_siblings() {
        let mut merged =
            parse_layers("", toml::Table::new(), "[sandbox]\nallow_read = [\"/tmp\"]\n[tools]\ndisabled = [\"web\"]\n")
                .unwrap();
        apply_env_from(&mut merged, |k| match k {
            "CONSTRUCT_SANDBOX" => Some("read-only".into()),
            "CONSTRUCT_TOOLS_WEB_SEARCH" => Some("ddg".into()),
            _ => None,
        });
        let cfg = into_config(merged).unwrap();
        assert_eq!(cfg.sandbox.mode, "read-only");
        assert_eq!(cfg.sandbox.allow_read, vec!["/tmp".to_string()]);
        assert_eq!(cfg.tools.disabled, vec!["web".to_string()]);
        assert_eq!(cfg.tools.web.search, "ddg");
        assert_eq!(cfg.provenance["sandbox.mode"], "env");
        assert_eq!(cfg.provenance["sandbox.allow_read"], "file");
        assert_eq!(cfg.provenance["tools.web.search"], "env");
    }

    #[test]
    fn file_overrides_baked_key_by_key_and_tables_merge() {
        let cfg = parse_two(
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
        let cfg = parse_two("", "model = \"m\"\n").unwrap();
        assert_eq!(cfg.model, "m");
        assert_eq!(cfg.provenance["model"], "file");
        let empty = parse_two("", "").unwrap();
        assert_eq!(empty.model, Config::default().model);
        assert!(empty.provenance.is_empty());
    }

    #[test]
    fn a_baked_window_pins_like_a_file_one() {
        assert!(parse_two("context_window = 8000\n", "").unwrap().window_pinned);
        assert!(parse_two("", "context_window = 8000\n").unwrap().window_pinned);
        assert!(!parse_two("model = \"m\"\n", "").unwrap().window_pinned);
    }

    #[test]
    fn arrays_replace_whole_not_append() {
        let cfg = parse_two("[tools]\ndisabled = [\"web\", \"mcp\"]\n", "[tools]\ndisabled = [\"web\"]\n").unwrap();
        assert_eq!(cfg.tools.disabled, vec!["web".to_string()]);
    }

    #[test]
    fn a_bad_layer_names_itself() {
        let err = parse_two("model = ", "").unwrap_err();
        assert!(format!("{err:#}").contains("baked layer"), "{err:#}");
        let err = parse_two("", "model = ").unwrap_err();
        assert!(format!("{err:#}").contains("file layer"), "{err:#}");
    }
}
