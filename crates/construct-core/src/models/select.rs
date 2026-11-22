//! Laying a selection into the running config: which provider and entry a
//! selector names, the window and cap that results, and the endpoint its
//! requests go to.
//!
//! The design choice (ADR 0027): a switch always lands on the same layers in
//! the same order, whatever was selected before: the top-level defaults,
//! what discovery found, the provider's defaults, then the user's entry.
//! The endpoint is never laid over anything; it is resolved on each call.

use super::{ModelEntry, Resolved};
use crate::config::Config;
use crate::endpoint::Endpoint;
use anyhow::Result;

/// Which layer set the selected model's window (`Config::window_source`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WindowSource {
    /// The user's model entry.
    Catalog,
    /// `[providers.<name>] context_window`.
    Provider(String),
    /// The provider's model list (discovery).
    Discovered,
    /// The top-level `context_window` in config.toml.
    Config,
    /// Nobody said: the built-in default.
    Default,
}

impl WindowSource {
    /// One word, for the manifest and the scorers that read it.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Catalog => "catalog",
            Self::Provider(_) => "provider",
            Self::Discovered => "discovered",
            Self::Config => "config",
            Self::Default => "default",
        }
    }
}

impl std::fmt::Display for WindowSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Catalog => f.write_str("catalog entry"),
            Self::Provider(p) => write!(f, "[providers.{p}]"),
            Self::Discovered => f.write_str("the server's model list"),
            Self::Config => f.write_str("config.toml"),
            Self::Default => f.write_str("the default"),
        }
    }
}

/// The top-level limits an entry can cover, as they were before any entry.
#[derive(Debug, Clone)]
pub struct Defaults {
    context_window: u32,
    max_tokens: Option<u32>,
}

impl Defaults {
    fn of(cfg: &Config) -> Self {
        Self { context_window: cfg.context_window, max_tokens: cfg.max_tokens }
    }

    fn restore(&self, cfg: &mut Config) {
        cfg.context_window = self.context_window;
        cfg.max_tokens = self.max_tokens;
    }
}

impl Config {
    /// Make `selector` the model: the defaults, then what discovery found for
    /// it, its provider's defaults, and its catalog entry, each over the last
    /// (ADR 0027 decision 6). A selector that resolves to more than one
    /// provider changes nothing.
    pub fn select_model(&mut self, selector: &str) -> Result<()> {
        let to = self.resolve(selector)?;
        let defaults = match &self.defaults {
            Some(d) => d.clone(),
            None => {
                let d = Box::new(Defaults::of(self));
                self.defaults = Some(d.clone());
                d
            }
        };
        defaults.restore(self);
        if let Some(d) = self.discovered_entry(selector).cloned() {
            d.apply(self);
        }
        if let Some(p) = to.provider.as_ref().and_then(|p| self.providers.get(p)) {
            self.context_window = p.context_window.unwrap_or(self.context_window);
            self.max_tokens = p.max_tokens.or(self.max_tokens);
        }
        self.model = selector.to_string();
        if let Some(e) = self.catalogued(selector).cloned() {
            e.apply(self);
        }
        Ok(())
    }

    /// The entry for `selector`: its provider's catalog; for `default`, the
    /// user's catalog, then the endpoint's own list (which also knows
    /// `vendor/model:variant` by `vendor/model`).
    pub fn lookup(&self, selector: &str) -> Option<&ModelEntry> {
        self.catalogued(selector).or_else(|| self.discovered_entry(selector))
    }

    /// Where the selected model's window came from, in `select_model`'s order
    /// read backwards: the first layer that set it.
    pub fn window_source(&self) -> WindowSource {
        let set = |e: Option<&ModelEntry>| e.is_some_and(|e| e.context_window.is_some());
        let provider =
            self.provider_name().filter(|p| self.providers.get(p).is_some_and(|d| d.context_window.is_some()));
        if set(self.catalogued(&self.model)) {
            WindowSource::Catalog
        } else if let Some(p) = provider {
            WindowSource::Provider(p)
        } else if set(self.discovered_entry(&self.model)) {
            WindowSource::Discovered
        } else if self.window_pinned {
            WindowSource::Config
        } else {
            WindowSource::Default
        }
    }

    /// The user's own entry for `selector`, in its provider's catalog.
    fn catalogued(&self, selector: &str) -> Option<&ModelEntry> {
        match self.resolve(selector).ok()? {
            Resolved { provider: Some(p), id } => self.providers.get(&p)?.models.get(&id),
            Resolved { provider: None, id } => self.models.get(&id),
        }
    }

    /// What its provider's list says about `selector`.
    fn discovered_entry(&self, selector: &str) -> Option<&ModelEntry> {
        let Resolved { provider, id } = self.resolve(selector).ok()?;
        let list = match provider {
            Some(p) => &self.providers.get(&p)?.discovered,
            None => &self.discovered,
        };
        list.get(&id)
            .or_else(|| id.split_once(':').and_then(|(base, _)| list.get(base)))
            .or_else(|| list.values().find(|e| e.aliases.iter().any(|a| a == &id)))
    }

    /// Where the selected model's requests go.
    pub fn endpoint(&self) -> Endpoint {
        self.endpoint_for(&self.model)
    }

    /// Where `selector`'s requests go: its provider's endpoint (the declared
    /// one for `default`), moved by its catalog entry if that names one.
    pub fn endpoint_for(&self, selector: &str) -> Endpoint {
        let named = self.resolve(selector).ok().and_then(|r| r.provider);
        let base = match named.as_ref().and_then(|p| self.providers.get(p).map(|d| (p, d))) {
            Some((name, p)) => p.endpoint(name),
            None => Endpoint::declared(self),
        };
        match self.lookup(selector) {
            Some(e) => e.endpoint(base),
            None => base,
        }
    }

    /// The catalog entry of the selected model.
    pub fn entry(&self) -> Option<&ModelEntry> {
        self.lookup(&self.model)
    }
}
