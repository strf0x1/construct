//! Where a model request goes: one endpoint's url, key, headers and dialect.
//!
//! The design choice (ADR 0027): an `Endpoint` is computed, never stored.
//! `Config` keeps what the user declared, and `Config::endpoint` resolves the
//! selected model against it on each call, so a model switch never has
//! anything to restore and nothing can go stale. Which key an endpoint gets is
//! decided here and nowhere else.
//!
//! This module sits below `config`, `providers` and `models` (ADR 0057): it
//! owns the endpoint wire settings (`ProviderConfig`, `Dialect`, `CacheMode`,
//! `Reasoning`) and depends only on `secrets`, never back on config resolution.

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::time::Duration;

/// The name of the top-level endpoint: the provider the top-level `base_url`,
/// `api_key_secret` and `[models.*]` declare.
pub const DEFAULT: &str = "default";

/// Default `[provider] idle_timeout` and `first_byte_timeout`, in seconds.
const IDLE_SECS: u64 = 120;
const FIRST_BYTE_SECS: u64 = 600;

/// What the endpoint needs beyond the plain OpenAI body (`llm::dialect`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProviderConfig {
    pub dialect: Dialect,
    pub cache: CacheMode,
    #[serde(skip_serializing_if = "Reasoning::is_echo")]
    pub reasoning: Reasoning,
    /// Seconds the stream may go silent once the model has started sending;
    /// then the call fails rather than hang the turn. Default 120.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idle_timeout: Option<u64>,
    /// Seconds to wait for the response and its first bytes. A local server
    /// can read a long prompt this long in silence. Default 600.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_byte_timeout: Option<u64>,
}

impl ProviderConfig {
    /// Resolve `Dialect::Auto` against `base_url`: openrouter.ai speaks
    /// OpenRouter, any other host speaks plain OpenAI.
    pub fn resolve_dialect(&self, base_url: &str) -> Dialect {
        match self.dialect {
            Dialect::Auto if host(base_url).is_some_and(|h| h == "openrouter.ai" || h.ends_with(".openrouter.ai")) => {
                Dialect::OpenRouter
            }
            Dialect::Auto => Dialect::OpenAi,
            d => d,
        }
    }
}

/// `"echo"` sends an assistant's reasoning back under the key the server
/// streamed it in (DeepSeek and Kimi reject a tool-calling history without
/// it; OpenRouter needs `reasoning_details` to keep signed thinking).
/// `"drop"` never sends it, for a server that rejects the field. ADR 0044.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Reasoning {
    #[default]
    Echo,
    Drop,
}

impl Reasoning {
    fn is_echo(&self) -> bool {
        *self == Reasoning::Echo
    }
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

#[derive(Debug, Clone, PartialEq)]
pub struct Endpoint {
    /// The provider: `default` for the top-level fields.
    pub name: String,
    /// Up to and including `/v1`, without a trailing slash.
    pub base_url: String,
    /// The secret holding this endpoint's key; empty means none is declared.
    api_key_secret: String,
    /// Extra headers sent with every request to this endpoint.
    pub headers: BTreeMap<String, String>,
    provider: ProviderConfig,
    /// Whether the key may come from `OPENROUTER_API_KEY` when the declared
    /// secret doesn't resolve: the top-level endpoint's old convenience, never
    /// granted to an endpoint a model entry moved to.
    fallback: bool,
}

impl Endpoint {
    /// The top-level endpoint, as `config.toml` declares it. Its key may fall
    /// back to `OPENROUTER_API_KEY` when a non-empty secret doesn't resolve.
    pub fn declared(
        base_url: &str,
        api_key_secret: &str,
        headers: BTreeMap<String, String>,
        provider: ProviderConfig,
    ) -> Self {
        Self {
            name: DEFAULT.to_string(),
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key_secret: api_key_secret.to_string(),
            headers,
            provider,
            fallback: true,
        }
    }

    /// A named provider's endpoint. Its key is its own secret, never a
    /// fallback.
    pub fn named(
        name: &str,
        base_url: &str,
        api_key_secret: &str,
        headers: BTreeMap<String, String>,
        provider: ProviderConfig,
    ) -> Self {
        let base_url = base_url.trim_end_matches('/').to_string();
        Self {
            name: name.to_string(),
            base_url,
            api_key_secret: api_key_secret.into(),
            headers,
            provider,
            fallback: false,
        }
    }

    /// The endpoint a model entry names: it takes only its own secret, and
    /// none of this endpoint's headers (they may carry its credential) or
    /// dialect (it is about this host). The key fallback stays only if the
    /// url is this one's.
    pub fn moved(&self, base_url: &str, api_key_secret: &str) -> Self {
        let base_url = base_url.trim_end_matches('/').to_string();
        Self {
            name: self.name.clone(),
            fallback: self.fallback && base_url == self.base_url,
            base_url,
            api_key_secret: api_key_secret.to_string(),
            headers: BTreeMap::new(),
            provider: ProviderConfig::default(),
        }
    }

    /// Apply a catalog entry's overrides on top of this endpoint: moving hosts
    /// drops headers and dialect (so credentials never leak across hosts),
    /// otherwise a non-empty secret replaces only the secret; explicit dialect
    /// or cache settings override the resulting endpoint's wire profile.
    pub fn with_model_overrides(
        &self,
        base_url: &str,
        api_key_secret: &str,
        dialect: Option<Dialect>,
        cache: Option<CacheMode>,
    ) -> Self {
        let mut ep = if !base_url.is_empty() {
            // A key found for one host is never sent to another.
            self.moved(base_url, api_key_secret)
        } else if !api_key_secret.is_empty() {
            let mut ep = self.clone();
            ep.api_key_secret = api_key_secret.to_string();
            ep
        } else {
            self.clone()
        };
        if let Some(d) = dialect {
            ep.provider.dialect = d;
        }
        if let Some(c) = cache {
            ep.provider.cache = c;
        }
        ep
    }

    /// The declared secret name, or `None` when this endpoint is keyless.
    pub fn key_secret(&self) -> Option<&str> {
        (!self.api_key_secret.is_empty()).then_some(self.api_key_secret.as_str())
    }

    /// The resolved wire dialect (`Dialect::Auto` resolved against `base_url`),
    /// cache mode and reasoning policy for requests to this endpoint.
    pub fn wire_dialect(&self) -> (Dialect, CacheMode, Reasoning) {
        (self.provider.resolve_dialect(&self.base_url), self.provider.cache, self.provider.reasoning)
    }

    /// How long a request may wait for its response and first bytes.
    pub fn first_byte_timeout(&self) -> Duration {
        Duration::from_secs(self.provider.first_byte_timeout.unwrap_or(FIRST_BYTE_SECS))
    }

    /// How long an open response stream may stay quiet between chunks.
    pub fn idle_timeout(&self) -> Duration {
        Duration::from_secs(self.provider.idle_timeout.unwrap_or(IDLE_SECS))
    }

    /// What an error from this endpoint starts with: nothing for `default`
    /// (its errors read as they always have), else the provider, its url and
    /// which key went with the request.
    pub fn error_prefix(&self) -> String {
        if self.name == DEFAULT {
            return String::new();
        }
        let key = match self.key_secret() {
            None => "no key".into(),
            Some(s) => format!("key {s}"),
        };
        format!("{} ({}, {key}): ", self.name, self.base_url)
    }

    /// The key for a session in this process: the keyring and environment.
    /// No secret declared means keyless, never the fallback.
    pub fn key(&self) -> Option<String> {
        match (self.key_secret(), self.fallback) {
            (None, _) => None,
            (Some(s), true) => crate::secrets::api_key(s),
            (Some(s), false) => crate::secrets::get(s),
        }
    }

    /// `key`, unless a secret is declared and nothing resolved: then nothing
    /// is sent, and the error names the secret (and the provider, if named).
    pub fn require(&self, key: Option<String>) -> Result<Option<String>> {
        if key.is_none()
            && let Some(secret) = self.key_secret()
        {
            let name = &self.name;
            if name == DEFAULT {
                bail!(crate::secrets::missing_key(secret));
            }
            bail!(
                "{name}: no API key: set {secret} (or construct secrets set {secret}); \
                 for a keyless endpoint set api_key_secret = \"\" under [providers.{name}]"
            );
        }
        Ok(key)
    }
}

fn host(url: &str) -> Option<&str> {
    let rest = url.split_once("://")?.1;
    let authority = rest.split('/').next()?;
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    Some(host.split(':').next().unwrap_or(host))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn declared() -> Endpoint {
        let headers = [("X-Title".to_string(), "c".to_string())].into();
        let provider = ProviderConfig { dialect: Dialect::OpenRouter, ..ProviderConfig::default() };
        Endpoint::declared("https://main.example/v1/", "MAIN_KEY", headers, provider)
    }

    #[test]
    fn another_host_keeps_nothing_of_this_one() {
        let main = declared();
        assert_eq!(main.base_url, "https://main.example/v1");
        let other = main.moved("http://127.0.0.1:8080/v1/", "");
        assert_eq!(other.base_url, "http://127.0.0.1:8080/v1");
        assert!(other.headers.is_empty() && other.provider == ProviderConfig::default());
        assert_eq!(other.key(), None, "no secret on another host means no key, not the fallback");
    }

    #[test]
    fn only_the_declared_host_keeps_the_fallback() {
        let main = declared();
        assert!(main.moved("https://main.example/v1/", "OTHER_KEY").fallback);
        assert!(!main.moved("https://else.example/v1", "OTHER_KEY").fallback);
    }

    #[test]
    fn a_declared_secret_that_resolves_to_nothing_is_an_error() {
        let ep = declared().moved("http://x/v1", "CONSTRUCT_TEST_UNSET_KEY");
        assert!(ep.require(None).unwrap_err().to_string().contains("CONSTRUCT_TEST_UNSET_KEY"));
        assert_eq!(ep.moved("http://y/v1", "").require(None).unwrap(), None, "keyless is fine");
    }

    #[test]
    fn wire_dialect_and_timeouts_apply_defaults_and_overrides() {
        let ep = Endpoint::declared("https://openrouter.ai/api/v1/", "", BTreeMap::new(), ProviderConfig::default());
        assert_eq!(ep.key_secret(), None);
        assert_eq!(ep.wire_dialect(), (Dialect::OpenRouter, CacheMode::Auto, Reasoning::Echo));
        assert_eq!(ep.first_byte_timeout(), Duration::from_secs(FIRST_BYTE_SECS));
        assert_eq!(ep.idle_timeout(), Duration::from_secs(IDLE_SECS));

        let custom = ep.with_model_overrides("", "CUSTOM_KEY", Some(Dialect::OpenAi), Some(CacheMode::Off));
        assert_eq!(custom.key_secret(), Some("CUSTOM_KEY"));
        assert_eq!(custom.wire_dialect(), (Dialect::OpenAi, CacheMode::Off, Reasoning::Echo));
    }
}
