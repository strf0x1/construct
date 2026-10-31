//! Where a model request goes: one endpoint's url, key, headers and dialect.
//!
//! The design choice (ADR 0027): an `Endpoint` is computed, never stored.
//! `Config` keeps what the user declared, and `Config::endpoint` resolves the
//! selected model against it on each call, so a model switch never has
//! anything to restore and nothing can go stale. Which key an endpoint gets is
//! decided here and nowhere else.

use crate::config::{Config, ProviderConfig};
use crate::providers::DEFAULT;
use anyhow::{Result, bail};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq)]
pub struct Endpoint {
    /// The provider: `default` for the top-level fields.
    pub name: String,
    /// Up to and including `/v1`, without a trailing slash.
    pub base_url: String,
    /// The secret holding this endpoint's key; empty means none is declared.
    pub api_key_secret: String,
    /// Extra headers sent with every request to this endpoint.
    pub headers: BTreeMap<String, String>,
    pub provider: ProviderConfig,
    /// Whether the key may come from `OPENROUTER_API_KEY` when the declared
    /// secret doesn't resolve: the top-level endpoint's old convenience, never
    /// granted to an endpoint a model entry moved to.
    fallback: bool,
}

impl Endpoint {
    /// The top-level endpoint, as `config.toml` declares it.
    pub fn declared(cfg: &Config) -> Self {
        Self {
            name: DEFAULT.to_string(),
            base_url: cfg.base_url.trim_end_matches('/').to_string(),
            api_key_secret: cfg.api_key_secret.clone(),
            headers: cfg.headers.clone(),
            provider: cfg.provider.clone(),
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

    /// The key for a session in this process: the keyring and environment.
    /// No secret declared means keyless, never the fallback.
    pub fn key(&self) -> Option<String> {
        match (self.api_key_secret.is_empty(), self.fallback) {
            (true, _) => None,
            (false, true) => crate::secrets::api_key(&self.api_key_secret),
            (false, false) => crate::secrets::get(&self.api_key_secret),
        }
    }

    /// The key for a delegated child: only what the parent put in its
    /// environment, under the declared name.
    pub fn env_key(&self) -> Option<String> {
        crate::secrets::env_key(&self.api_key_secret)
    }

    /// `key`, unless a secret is declared and nothing resolved: then nothing
    /// is sent, and the error names the secret (and the provider, if named).
    pub fn require(&self, key: Option<String>) -> Result<Option<String>> {
        if key.is_none() && !self.api_key_secret.is_empty() {
            let (name, secret) = (&self.name, &self.api_key_secret);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn declared() -> Endpoint {
        let cfg: Config = toml::from_str(
            "base_url = \"https://main.example/v1/\"\napi_key_secret = \"MAIN_KEY\"\n[headers]\nX-Title = \"c\"\n\
             [provider]\ndialect = \"openrouter\"\n",
        )
        .unwrap();
        Endpoint::declared(&cfg)
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
}
