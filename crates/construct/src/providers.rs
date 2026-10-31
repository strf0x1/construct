//! Named providers: `[providers.<name>]` tables, the built-in presets, and
//! how a model selector picks one (ADR 0027).
//!
//! The design choice: a selector is resolved without looking at any fetched
//! model list. A first segment that names a configured provider picks it and
//! the rest is the id; anything else is a bare id, which goes to the one
//! provider whose catalog lists it, and to the implicit `default` provider
//! (the top-level fields) when none does. So the same config always routes
//! the same way, cached list or not.

use crate::config::Config;
use crate::endpoint::Endpoint;
use crate::models::ModelEntry;
use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The provider the top-level `base_url`, `api_key_secret` and `[models.*]`
/// declare.
pub const DEFAULT: &str = "default";

/// What the endpoint needs beyond the plain OpenAI body (`llm::dialect`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProviderConfig {
    pub dialect: Dialect,
    pub cache: CacheMode,
}

/// `"auto"` picks by `base_url`: openrouter.ai is `"openrouter"`, anything
/// else `"openai"`, the plain dialect every compatible server speaks (local
/// servers, vLLM, DeepSeek, OpenAI). Name one when a proxy hides the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Dialect {
    #[default]
    Auto,
    #[serde(rename = "openai")]
    OpenAi,
    #[serde(rename = "openrouter")]
    OpenRouter,
}

/// `"auto"` marks prompt-cache breakpoints for models whose provider only
/// caches when asked (Anthropic and Gemini behind OpenRouter); `"off"` never
/// marks any. Providers that cache on their own are unaffected either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CacheMode {
    #[default]
    Auto,
    Off,
}

/// How a provider's models and windows are learned (`llm::discover`).
/// `openai` reads `/models` (also vLLM's); `llama.cpp` adds the loaded
/// context from `/props`; `ollama` reads `/api/tags` and `/api/show`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Discover {
    #[default]
    #[serde(alias = "vllm")]
    Openai,
    #[serde(rename = "llama.cpp", alias = "llamacpp")]
    LlamaCpp,
    Ollama,
    Off,
}

/// One `[providers.<name>]` table. Everything is optional for a preset.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Provider {
    /// Up to and including `/v1`. A preset knows its own.
    pub base_url: String,
    /// The secret holding the key. Absent: the preset's, none for a loopback
    /// url, else `<NAME>_API_KEY`. `""` means keyless.
    pub api_key_secret: Option<String>,
    pub headers: BTreeMap<String, String>,
    pub dialect: Option<Dialect>,
    pub cache: Option<CacheMode>,
    /// How to learn its models and their windows; `off` asks nothing.
    pub discover: Discover,
    /// Defaults for this provider's models, under any model entry. A
    /// `context_window` here wins over a discovered one.
    pub context_window: Option<u32>,
    pub max_tokens: Option<u32>,
    /// This provider's catalog. An entry here is served by the provider, so
    /// it names no `base_url` or `api_key_secret` of its own.
    pub models: BTreeMap<String, ModelEntry>,
    /// What discovery found (`llm::overlay::overlay`); `models` wins an id
    /// in both, and resolution never looks here (decision 5).
    #[serde(skip)]
    pub discovered: BTreeMap<String, ModelEntry>,
}

/// A built-in provider: one row of data, never a code path.
struct Preset {
    name: &'static str,
    base_url: &'static str,
    secret: &'static str,
    dialect: Dialect,
    /// Ids under this prefix are the provider's own routers, and a rest with
    /// no `/` is one of them (`openrouter/free`).
    routers: Option<&'static str>,
}

const PRESETS: &[Preset] = &[Preset {
    name: "openrouter",
    base_url: "https://openrouter.ai/api/v1",
    secret: "OPENROUTER_API_KEY",
    dialect: Dialect::OpenRouter,
    routers: Some("openrouter/"),
}];

fn preset(name: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|p| p.name == name)
}

/// Where a selector goes: a named provider (`None` is `default`) and the id
/// the request carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub provider: Option<String>,
    pub id: String,
}

impl Provider {
    /// The endpoint `name`'s requests go to.
    pub fn endpoint(&self, name: &str) -> Endpoint {
        let p = preset(name);
        let base_url = if self.base_url.is_empty() { p.map_or("", |p| p.base_url) } else { &self.base_url };
        let secret = match (&self.api_key_secret, p) {
            (Some(s), _) => s.clone(),
            (None, Some(p)) => p.secret.to_string(),
            (None, None) if loopback(base_url) => String::new(),
            (None, None) => format!("{}_API_KEY", name.to_uppercase().replace('-', "_")),
        };
        let provider = ProviderConfig {
            dialect: self.dialect.or(p.map(|p| p.dialect)).unwrap_or_default(),
            cache: self.cache.unwrap_or_default(),
        };
        Endpoint::named(name, base_url, &secret, self.headers.clone(), provider)
    }

    fn check(&self, name: &str) -> Result<()> {
        ensure!(
            !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "[providers.{name:?}]: a provider name is letters, digits, - and _"
        );
        ensure!(name != DEFAULT, "[providers.default]: `default` is the top-level endpoint; give this one a name");
        ensure!(
            !self.base_url.is_empty() || preset(name).is_some(),
            "[providers.{name}] needs a base_url (only {} are built in)",
            PRESETS.iter().map(|p| p.name).collect::<Vec<_>>().join(", ")
        );
        for (id, e) in &self.models {
            let at = format!("[providers.{name}.models.{id:?}]");
            ensure!(e.base_url.is_empty() && e.api_key_secret.is_empty(), "{at}: the provider's url and key serve it");
            e.check(id).map_err(|err| anyhow::anyhow!("{at}: {err}"))?;
        }
        Ok(())
    }
}

/// Whether `url` names this machine, where a server usually takes no key.
fn loopback(url: &str) -> bool {
    let host = url.split_once("://").map_or(url, |(_, r)| r).split('/').next().unwrap_or_default();
    let host = host.rsplit_once(':').filter(|(h, _)| !h.ends_with(':')).map_or(host, |(h, _)| h);
    matches!(host, "localhost" | "127.0.0.1" | "[::1]") || host.ends_with(".localhost")
}

impl Config {
    /// Which provider serves `selector`, and the id it is sent as. An id that
    /// more than one provider lists must be qualified.
    pub fn resolve(&self, selector: &str) -> Result<Resolved> {
        if let Some((first, rest)) = selector.split_once('/') {
            if first == DEFAULT {
                return Ok(Resolved { provider: None, id: rest.to_string() });
            }
            if self.providers.contains_key(first) {
                let id = match preset(first).and_then(|p| p.routers) {
                    Some(prefix) if !rest.contains('/') => format!("{prefix}{rest}"),
                    _ => rest.to_string(),
                };
                return Ok(Resolved { provider: Some(first.to_string()), id });
            }
        }
        let mut named = self.providers.iter().filter(|(_, p)| p.models.contains_key(selector)).map(|(n, _)| n);
        // Only what the user catalogued: a fetched list never changes where an id goes.
        let default_lists = self.models.contains_key(selector);
        match (named.next(), named.next(), default_lists) {
            (Some(n), None, false) => Ok(Resolved { provider: Some(n.clone()), id: selector.to_string() }),
            (None, _, _) => Ok(Resolved { provider: None, id: selector.to_string() }),
            _ => {
                let mut all: Vec<String> = self
                    .providers
                    .iter()
                    .filter(|(_, p)| p.models.contains_key(selector))
                    .map(|(n, _)| format!("{n}/{selector}"))
                    .collect();
                if default_lists {
                    all.insert(0, format!("{DEFAULT}/{selector}"));
                }
                bail!("{selector:?} is served by more than one provider; pick one: {}", all.join(", "))
            }
        }
    }

    /// The id `selector` is sent as: the selector itself if it doesn't resolve.
    pub fn wire_id(&self, selector: &str) -> String {
        self.resolve(selector).map_or_else(|_| selector.to_string(), |r| r.id)
    }

    /// The named provider serving the selected model; `None` for `default`.
    pub fn provider_name(&self) -> Option<String> {
        self.resolve(&self.model).ok().and_then(|r| r.provider)
    }

    /// How provider `name` learns its models: `openai` for `default`.
    pub fn discovery(&self, name: &str) -> Discover {
        self.providers.get(name).map_or(Discover::Openai, |p| p.discover)
    }

    /// Every provider's name, `default` first.
    pub fn provider_names(&self) -> Vec<String> {
        std::iter::once(DEFAULT.to_string()).chain(self.providers.keys().cloned()).collect()
    }

    /// Provider `name`'s endpoint and the ids its catalog lists (as sent).
    pub fn provider(&self, name: &str) -> Option<(Endpoint, Vec<String>)> {
        if name == DEFAULT {
            return Some((Endpoint::declared(self), self.models.keys().cloned().collect()));
        }
        let p = self.providers.get(name)?;
        Some((p.endpoint(name), p.models.keys().cloned().collect()))
    }

    /// Whether the selected model is one of OpenRouter's own routers, judged
    /// on the id sent, not the selector (`openrouter/anthropic/x` is not one).
    pub fn on_router(&self) -> bool {
        crate::models::is_router(&self.wire_id(&self.model))
    }

    /// The selected model as `provider/id` when a named provider serves it,
    /// so a bare id that resolved elsewhere says where.
    pub fn qualified_model(&self) -> String {
        match self.provider_name() {
            Some(p) if !self.model.starts_with(&format!("{p}/")) => format!("{p}/{}", self.model),
            _ => self.model.clone(),
        }
    }

    /// Where requests go, for display: `name · url`, or the url for `default`.
    pub fn endpoint_label(&self) -> String {
        let ep = self.endpoint();
        if ep.name == DEFAULT { ep.base_url } else { format!("{} · {}", ep.name, ep.base_url) }
    }

    /// Every catalogued selector, for the picker and the palette: the
    /// top-level catalog, then each provider's as `name/id`, then the
    /// endpoint's own list.
    pub fn catalog_ids(&self) -> Vec<String> {
        // A top-level id another provider also lists is shown qualified.
        let plain = |id: &String| if self.resolve(id).is_ok() { id.clone() } else { format!("{DEFAULT}/{id}") };
        let mut ids: Vec<String> = self.models.keys().map(plain).collect();
        for (name, p) in &self.providers {
            ids.extend(p.models.keys().map(|id| format!("{name}/{id}")));
            ids.extend(p.discovered.keys().filter(|id| !p.models.contains_key(*id)).map(|id| format!("{name}/{id}")));
        }
        ids.extend(self.discovered.keys().filter(|id| !self.models.contains_key(*id)).map(plain));
        ids
    }

    /// The `[providers.*]` tables and every model setting, checked at load.
    pub(crate) fn check_providers(&self) -> Result<()> {
        for (name, p) in &self.providers {
            p.check(name)?;
        }
        let agents = self.agents.iter().map(|(n, a)| (format!("[agents.{n}] model"), &a.model));
        let named = [("model".to_string(), &self.model), ("[learn] model".into(), &self.learn.model)]
            .into_iter()
            .chain([("[compaction] model".to_string(), &self.compaction.model)])
            .chain(agents);
        for (field, selector) in named.filter(|(_, s)| !s.is_empty()) {
            self.resolve(selector).map_err(|e| anyhow::anyhow!("{field}: {e}"))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(toml: &str) -> Config {
        let c: Config = toml::from_str(toml).unwrap();
        c.check_providers().unwrap();
        c
    }

    const TWO: &str = r#"
        [models."shared"]
        [providers.local]
        base_url = "http://127.0.0.1:8080/v1"
        [providers.local.models."qwen3-8b"]
        context_window = 32768
        [providers.local.models."shared"]
        [providers.gpubox]
        base_url = "http://10.0.0.5:8000/v1"
        [providers.gpubox.models."qwen3-8b"]
        [providers.openrouter]
    "#;

    fn r(provider: Option<&str>, id: &str) -> Resolved {
        Resolved { provider: provider.map(String::from), id: id.into() }
    }

    #[test]
    fn a_named_first_segment_picks_the_provider() {
        let c = cfg(TWO);
        assert_eq!(c.resolve("local/qwen3-8b").unwrap(), r(Some("local"), "qwen3-8b"));
        assert_eq!(c.resolve("gpubox/qwen3-8b").unwrap(), r(Some("gpubox"), "qwen3-8b"));
        assert_eq!(c.resolve("local/not-listed").unwrap(), r(Some("local"), "not-listed"), "listing is not required");
        assert_eq!(c.resolve("default/shared").unwrap(), r(None, "shared"));
    }

    #[test]
    fn openrouter_keeps_vendor_ids_and_reads_a_bare_rest_as_its_router() {
        let c = cfg(TWO);
        assert_eq!(c.resolve("openrouter/anthropic/claude-x").unwrap(), r(Some("openrouter"), "anthropic/claude-x"));
        assert_eq!(c.resolve("openrouter/free").unwrap(), r(Some("openrouter"), "openrouter/free"));
        assert_eq!(c.resolve("openrouter/openrouter/free").unwrap(), r(Some("openrouter"), "openrouter/free"));
        let without = cfg("");
        assert_eq!(without.resolve("openrouter/free").unwrap(), r(None, "openrouter/free"), "no provider: a bare id");
    }

    #[test]
    fn a_bare_id_goes_where_it_is_listed_or_to_default() {
        let c = cfg(TWO);
        assert_eq!(c.resolve("vendor/unknown").unwrap(), r(None, "vendor/unknown"));
        let only = cfg("[providers.local]\nbase_url = \"http://127.0.0.1:1/v1\"\n[providers.local.models.\"m\"]\n");
        assert_eq!(only.resolve("m").unwrap(), r(Some("local"), "m"));
    }

    #[test]
    fn an_ambiguous_bare_id_is_refused_with_the_candidates() {
        let c = cfg(TWO);
        let err = c.resolve("qwen3-8b").unwrap_err().to_string();
        assert!(err.contains("gpubox/qwen3-8b") && err.contains("local/qwen3-8b"), "{err}");
        let err = c.resolve("shared").unwrap_err().to_string();
        assert!(err.contains("default/shared") && err.contains("local/shared"), "{err}");
        let bad: Config = toml::from_str(&format!("model = \"qwen3-8b\"\n{TWO}")).unwrap();
        assert!(bad.check_providers().unwrap_err().to_string().starts_with("model: "));
    }

    #[test]
    fn keys_default_by_preset_by_loopback_and_by_name() {
        let c = cfg(TWO);
        let ep = |n: &str| c.providers[n].endpoint(n);
        assert_eq!(ep("openrouter").base_url, "https://openrouter.ai/api/v1");
        assert_eq!(ep("openrouter").api_key_secret, "OPENROUTER_API_KEY");
        assert_eq!(ep("openrouter").provider.dialect, Dialect::OpenRouter);
        assert_eq!(ep("local").api_key_secret, "", "a loopback server is keyless unless it says so");
        assert_eq!(ep("gpubox").api_key_secret, "GPUBOX_API_KEY");
        let keyless = cfg("[providers.lan-box]\nbase_url = \"http://10.0.0.9/v1\"\napi_key_secret = \"\"\n");
        assert_eq!(keyless.providers["lan-box"].endpoint("lan-box").api_key_secret, "");
        let named = cfg("[providers.lan-box]\nbase_url = \"http://10.0.0.9/v1\"\n");
        assert_eq!(named.providers["lan-box"].endpoint("lan-box").api_key_secret, "LAN_BOX_API_KEY");
    }

    #[test]
    fn loopback_hosts() {
        for u in ["http://127.0.0.1:8000/v1", "http://localhost/v1", "http://[::1]:8080/v1", "http://ai.localhost/v1"] {
            assert!(loopback(u), "{u}");
        }
        for u in ["http://10.0.0.5:8000/v1", "https://openrouter.ai/api/v1", "http://127.0.0.1.evil.example/v1"] {
            assert!(!loopback(u), "{u}");
        }
    }

    #[test]
    fn mistakes_name_the_table() {
        let bad = |toml: &str| toml::from_str::<Config>(toml).unwrap().check_providers().unwrap_err().to_string();
        assert!(bad("[providers.x]\n").contains("[providers.x] needs a base_url"));
        assert!(bad("[providers.default]\nbase_url = \"http://a/v1\"\n").contains("top-level"));
        assert!(bad("[providers.\"a/b\"]\nbase_url = \"http://a/v1\"\n").contains("a provider name"));
        let entry = "[providers.x]\nbase_url = \"http://a/v1\"\n[providers.x.models.m]\nbase_url = \"http://b/v1\"\n";
        assert!(bad(entry).contains("[providers.x.models.\"m\"]"));
        assert!(toml::from_str::<Config>("[providers.x]\nbase_ur = \"\"\n").is_err(), "typos are refused");
    }
}
