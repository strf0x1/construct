//! The hand-written model catalog: what a model id means to this harness
//! (its window, output cap, endpoint, price). Entries come from
//! `$CONSTRUCT_HOME/models.toml` and `[models.*]` in `config.toml`.
//!
//! The one design choice: the top-level `Config` fields stay the defaults,
//! and `Config::select_model` lays an entry over them, remembering what it
//! covered. So a switch always lands on "defaults plus that entry", whatever
//! was selected before, and an id the catalog doesn't know gets the defaults
//! again. Field names follow OpenRouter's `/models` (`context_length`,
//! `top_provider.max_completion_tokens`, `pricing`) so a later auto-fill is a
//! straight mapping.

use crate::config::{CacheMode, Config, Dialect, ProviderConfig};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// One catalog entry. Everything is optional: an entry names only what
/// differs from the top-level settings.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ModelEntry {
    /// One line for the picker: `"Haiku 5.5 — fast recon"`.
    pub label: String,
    /// A longer note, shown beside the list.
    pub description: String,
    /// Short facts (`"reasoning"`, `"vision"`), shown as chips and searched.
    pub tags: Vec<String>,
    /// Tokens the model can take; compaction and the `ctx` meter plan
    /// against it. Absent means the top-level `context_window`.
    pub context_window: Option<u32>,
    /// Output cap per request. Absent means the top-level `max_tokens`.
    pub max_tokens: Option<u32>,
    /// A different endpoint for this model. It takes its own key and none
    /// of the defaults' credentials: the extra `headers` stay behind, and
    /// without `api_key_secret` the endpoint gets no key (a local server).
    pub base_url: String,
    /// The secret holding this model's endpoint key.
    pub api_key_secret: String,
    pub dialect: Option<Dialect>,
    pub cache: Option<CacheMode>,
    /// Used for the cost figure when the provider reports none.
    pub pricing: Option<Pricing>,
}

/// US dollars per million tokens (OpenRouter quotes per token; multiply by
/// 1e6 when filling these in from its `pricing`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pricing {
    pub prompt: f64,
    pub completion: f64,
}

impl Pricing {
    /// What a request cost at these prices. Cached tokens are billed as
    /// plain input: an overestimate, which is the safer direction.
    pub fn cost(&self, prompt_tokens: u64, completion_tokens: u64) -> f64 {
        (prompt_tokens as f64 * self.prompt + completion_tokens as f64 * self.completion) / 1e6
    }
}

impl ModelEntry {
    fn check(&self, id: &str) -> Result<()> {
        ensure!(!id.trim().is_empty(), "a model id is empty");
        ensure!(self.context_window != Some(0), "[models.{id:?}] context_window must be more than 0");
        ensure!(self.max_tokens != Some(0), "[models.{id:?}] max_tokens must be more than 0");
        if let (Some(w), Some(m)) = (self.context_window, self.max_tokens) {
            ensure!(m < w, "[models.{id:?}] max_tokens ({m}) must be under context_window ({w})");
        }
        Ok(())
    }

    /// Lay this entry over `cfg`.
    fn apply(&self, cfg: &mut Config) {
        if let Some(w) = self.context_window {
            cfg.context_window = w;
        }
        if self.max_tokens.is_some() {
            cfg.max_tokens = self.max_tokens;
        }
        if !self.base_url.is_empty() {
            cfg.base_url = self.base_url.trim_end_matches('/').to_string();
            cfg.headers.clear();
            cfg.provider = ProviderConfig::default();
            // A key found for one host is never sent to another.
            cfg.api_key_secret = self.api_key_secret.clone();
        } else if !self.api_key_secret.is_empty() {
            cfg.api_key_secret = self.api_key_secret.clone();
        }
        if let Some(d) = self.dialect {
            cfg.provider.dialect = d;
        }
        if let Some(c) = self.cache {
            cfg.provider.cache = c;
        }
    }
}

/// The top-level values an entry can cover, as they were before any entry.
#[derive(Debug, Clone)]
pub struct Defaults {
    context_window: u32,
    max_tokens: Option<u32>,
    base_url: String,
    api_key_secret: String,
    headers: BTreeMap<String, String>,
    provider: ProviderConfig,
}

impl Defaults {
    fn of(cfg: &Config) -> Self {
        Self {
            context_window: cfg.context_window,
            max_tokens: cfg.max_tokens,
            base_url: cfg.base_url.clone(),
            api_key_secret: cfg.api_key_secret.clone(),
            headers: cfg.headers.clone(),
            provider: cfg.provider.clone(),
        }
    }

    fn restore(&self, cfg: &mut Config) {
        cfg.context_window = self.context_window;
        cfg.max_tokens = self.max_tokens;
        cfg.base_url = self.base_url.clone();
        cfg.api_key_secret = self.api_key_secret.clone();
        cfg.headers = self.headers.clone();
        cfg.provider = self.provider.clone();
    }
}

impl Config {
    /// Make `id` the model: the defaults, then its catalog entry if it has one.
    pub fn select_model(&mut self, id: &str) {
        let defaults = match &self.defaults {
            Some(d) => d.clone(),
            None => {
                let d = Box::new(Defaults::of(self));
                self.defaults = Some(d.clone());
                d
            }
        };
        defaults.restore(self);
        self.model = id.to_string();
        if let Some(e) = self.lookup(id).cloned() {
            e.apply(self);
        }
    }

    /// The entry for `id`: the user's catalog, then the endpoint's own list
    /// (which also knows `vendor/model:variant` by `vendor/model`).
    pub fn lookup(&self, id: &str) -> Option<&ModelEntry> {
        self.models.get(id).or_else(|| self.discovered.get(id)).or_else(|| {
            let (base, _) = id.split_once(':')?;
            self.discovered.get(base)
        })
    }

    /// The key for the selected model's endpoint. Where an entry moved the
    /// endpoint off the default one, only the entry's own secret counts: no
    /// OpenRouter fallback, which would hand that key to the other host.
    pub fn endpoint_key(&self) -> Option<String> {
        let moved = self.defaults.as_ref().is_some_and(|d| d.base_url != self.base_url);
        match (moved, self.api_key_secret.is_empty()) {
            (true, true) => None,
            (true, false) => crate::secrets::get(&self.api_key_secret),
            (false, _) => crate::secrets::api_key(&self.api_key_secret),
        }
    }

    /// The catalog entry of the selected model.
    pub fn entry(&self) -> Option<&ModelEntry> {
        self.lookup(&self.model)
    }

    /// Fold `models.toml` into `[models.*]`; on a clash `config.toml` wins,
    /// being the file the user edits for this machine.
    pub(crate) fn load_catalog(&mut self, file: &Path) -> Result<()> {
        let mut merged = read_file(file)?;
        merged.extend(std::mem::take(&mut self.models));
        for (id, e) in &merged {
            e.check(id).with_context(|| format!("in {} or config.toml", file.display()))?;
        }
        self.models = merged;
        Ok(())
    }
}

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct File {
    models: BTreeMap<String, ModelEntry>,
}

fn read_file(path: &Path) -> Result<BTreeMap<String, ModelEntry>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(toml::from_str::<File>(&text).with_context(|| format!("parsing {}", path.display()))?.models),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// A window as a list shows it: `32.8k`, `200k`, `1M`, `1.1M`.
pub fn short_tokens(n: u64) -> String {
    match n {
        0 => "—".into(),
        1_000_000.. => format!("{}M", tenths(n as f64 / 1e6)),
        100_000.. => format!("{}k", n / 1000),
        1_000.. => format!("{}k", tenths(n as f64 / 1e3)),
        n => n.to_string(),
    }
}

/// One decimal, dropped when it is a zero.
fn tenths(x: f64) -> String {
    let s = format!("{x:.1}");
    s.strip_suffix(".0").map_or(s.clone(), String::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_with(toml: &str) -> Config {
        let mut c: Config = toml::from_str(toml).unwrap();
        c.load_catalog(Path::new("/nonexistent/models.toml")).unwrap();
        c.select_model(&c.model.clone());
        c
    }

    const CATALOG: &str = r#"
        model = "big"
        context_window = 131072
        max_tokens = 4000
        [models.big]
        context_window = 1000000
        max_tokens = 32000
        [models.small]
        label = "Small"
        context_window = 32768
        base_url = "https://other.example/v1/"
        api_key_secret = "OTHER_KEY"
        dialect = "openai"
        [models.small.pricing]
        prompt = 1.0
        completion = 5.0
    "#;

    #[test]
    fn an_entry_covers_the_defaults_and_unknown_ids_get_them_back() {
        let mut c = cfg_with(CATALOG);
        assert_eq!((c.context_window, c.max_tokens), (1_000_000, Some(32000)));
        c.select_model("small");
        assert_eq!(c.context_window, 32768);
        assert_eq!(c.max_tokens, Some(4000), "an entry that names no cap leaves the default");
        assert_eq!(c.base_url, "https://other.example/v1");
        assert_eq!(c.api_key_secret, "OTHER_KEY");
        c.select_model("some/other-id");
        assert_eq!((c.context_window, c.max_tokens), (131_072, Some(4000)));
        assert_eq!(c.base_url, Config::default().base_url, "the endpoint is back too");
        assert_eq!(c.api_key_secret, Config::default().api_key_secret);
        assert!(c.entry().is_none());
    }

    #[test]
    fn another_endpoint_without_a_secret_gets_no_key() {
        let mut c =
            cfg_with("api_key_secret = \"MAIN_KEY\"\n[models.local]\nbase_url = \"http://127.0.0.1:8080/v1\"\n");
        c.select_model("local");
        assert_eq!(c.api_key_secret, "", "the default's secret is not sent to another host");
        assert!(c.endpoint_key().is_none(), "and neither is the OpenRouter fallback");
    }

    #[test]
    fn another_endpoint_leaves_the_extra_headers_behind() {
        let mut c = cfg_with(&format!("{CATALOG}\n[headers]\nX-Title = \"construct\"\n"));
        c.select_model("small");
        assert!(c.headers.is_empty());
        c.select_model("big");
        assert_eq!(c.headers.len(), 1);
    }

    #[test]
    fn mistakes_in_the_file_are_named() {
        let bad = |toml: &str| {
            let mut c: Config = toml::from_str(toml).unwrap_or_default();
            c.load_catalog(Path::new("/nonexistent")).err().map(|e| format!("{e:#}"))
        };
        assert!(bad("[models.x]\ncontext_window = 0").unwrap().contains("context_window"));
        assert!(bad("[models.x]\ncontext_window = 1000\nmax_tokens = 2000").unwrap().contains("max_tokens"));
        assert!(toml::from_str::<Config>("[models.x]\ncontext_lenght = 5").is_err(), "typos are refused");
    }

    #[test]
    fn config_toml_wins_a_clash_with_models_toml() {
        let dir = std::env::temp_dir().join(format!("construct-models-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("models.toml");
        std::fs::write(&file, "[models.a]\nlabel = \"from file\"\n[models.b]\nlabel = \"only file\"\n").unwrap();
        let mut c: Config = toml::from_str("[models.a]\nlabel = \"from config\"").unwrap();
        c.load_catalog(&file).unwrap();
        assert_eq!(c.models["a"].label, "from config");
        assert_eq!(c.models["b"].label, "only file");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn pricing_and_token_shapes() {
        assert_eq!(Pricing { prompt: 1.0, completion: 5.0 }.cost(2_000_000, 100_000), 2.5);
        assert_eq!(short_tokens(200_000), "200k");
        assert_eq!(short_tokens(1_048_576), "1M");
        assert_eq!(short_tokens(1_100_000), "1.1M");
        assert_eq!(short_tokens(131_072), "131k");
        assert_eq!(short_tokens(32_768), "32.8k");
        assert_eq!(short_tokens(0), "—");
    }
}
